//! The seams the pipeline runs against.
//!
//! Every stage that touches hardware, a GPU, a network, or the user's desktop
//! is reached through a trait here, with the production adapter and a test
//! double side by side. That is not abstraction for its own sake — it is what
//! makes the daemon's *concurrency* testable at all:
//!
//! - CI has no microphone, so `AudioCapture::new()` would fail before the
//!   state machine ever ran;
//! - CI has no CUDA device and no 1.5GB GGUF, so a real transcription is not
//!   an option;
//! - "cancel arriving mid-`Injecting`" is a sub-100ms window against a real
//!   injector, which is untestable by timing but trivial against a double that
//!   blocks until the test says go.
//!
//! The traits are deliberately narrow — they describe what the pipeline needs,
//! not what the underlying crate offers. S12 provides the model-manager-backed
//! [`SttProvider`] from `dictate-stt`, and S13 replaces
//! [`TextInjector`] with the per-app policy engine; neither needs to touch the
//! engine to do it.
//!
//! # Why the audio adapter owns a thread
//!
//! `cpal::Stream` is `!Send` on ALSA, so `AudioCapture` cannot be moved into a
//! spawned task or shared behind an `Arc`. Today's daemon gets away with it by
//! keeping everything inside one `block_on` future; a control plane that
//! serves concurrent connections cannot. [`HostAudioSource`] therefore confines
//! the capture to a dedicated thread and talks to it over channels, which is
//! the only arrangement that makes the rest of the daemon `Send`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use dictate_audio::{AudioCapture, CaptureDiagnostics, EarconCue, EarconPlayer, Level, LevelMeter};
pub use dictate_fmt::FormatContext;
use dictate_proto::InjectionOutcome;
pub use dictate_stt::{BoxFuture, ModelInfo, SttProvider, SttRequest, Transcription, WhisperStt};
pub use dictate_vad::{GateDecision, TrailingSilenceTracker, VoiceActivityGate};

// ---------------------------------------------------------------------------
// Audio capture
// ---------------------------------------------------------------------------

/// Microphone capture.
pub trait AudioSource: Send + Sync + 'static {
    /// Begin capturing. Errors if a device is unavailable or already running.
    fn start(&self) -> BoxFuture<'_, Result<()>>;

    /// Stop capturing and take the buffer. `None` means nothing was captured.
    ///
    /// Includes the trailing-capture delay that keeps the last syllable from
    /// being clipped, so this is *not* instantaneous — the pipeline races it
    /// against cancellation.
    fn stop(&self) -> BoxFuture<'_, Option<Vec<f32>>>;

    /// Stop capturing and discard the buffer. Must be safe to call when not
    /// recording.
    fn cancel(&self);

    /// Whether capture is currently running.
    fn is_recording(&self) -> bool;

    /// Copy audio captured so far without stopping the stream. This is only
    /// used by hands-free VAD auto-stop; regular recording never snapshots.
    fn snapshot(&self) -> BoxFuture<'_, Option<Vec<f32>>>;

    /// Diagnostics from the most recently stopped capture. Defaulting to an
    /// empty value preserves every third-party/test `AudioSource` implementation.
    fn diagnostics(&self) -> CaptureDiagnostics {
        CaptureDiagnostics::default()
    }

    /// Why this source can never record — a configuration choice, such as
    /// audio-less mode — or `None` when it can. Checked before a session is
    /// created, so the caller gets a clear reason instead of a device error.
    fn unavailable_reason(&self) -> Option<String> {
        None
    }

    /// The microphone's state for `get_status`, including whether an input
    /// device is open *right now*. `None` when the source does not report it.
    fn input_status(&self) -> Option<dictate_proto::AudioStatus> {
        None
    }

    /// Input level since the previous call, while recording — what the
    /// pipeline publishes as `audio_level` for HUD meters. `None` (the
    /// default) publishes nothing.
    fn level(&self) -> Option<Level> {
        None
    }
}

/// The capture source of a daemon started with `[audio] capture = false`.
///
/// It exists so that audio-less mode is a property of the pipeline's wiring
/// rather than a flag every stage has to remember: nothing here can open an
/// input device, because nothing here knows how to.
#[derive(Debug, Clone)]
pub struct DisabledAudioSource {
    pre_roll_ms: u32,
}

impl DisabledAudioSource {
    /// The reason reported to a client that tries to record.
    pub const REASON: &'static str =
        "audio capture is disabled on this daemon ([audio] capture = false); \
         transcribe a file with `dictate transcribe <file.wav>`, or set capture = true and restart";

    /// Build from the (otherwise unused) audio config.
    #[must_use]
    pub fn new(config: &crate::config::AudioConfig) -> Self {
        Self {
            pre_roll_ms: config.pre_roll_ms,
        }
    }
}

impl AudioSource for DisabledAudioSource {
    fn start(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(anyhow::anyhow!(Self::REASON)) })
    }

    fn stop(&self) -> BoxFuture<'_, Option<Vec<f32>>> {
        Box::pin(async { None })
    }

    fn cancel(&self) {}

    fn is_recording(&self) -> bool {
        false
    }

    fn snapshot(&self) -> BoxFuture<'_, Option<Vec<f32>>> {
        Box::pin(async { None })
    }

    fn unavailable_reason(&self) -> Option<String> {
        Some(Self::REASON.to_string())
    }

    fn input_status(&self) -> Option<dictate_proto::AudioStatus> {
        Some(dictate_proto::AudioStatus {
            capture_enabled: false,
            input_open: false,
            pre_roll_ms: Some(self.pre_roll_ms),
        })
    }
}

/// Sample rate the pipeline captures and transcribes at.
pub const SAMPLE_RATE_HZ: f64 = 16_000.0;

/// Duration in milliseconds of a 16 kHz mono buffer.
#[must_use]
pub fn audio_ms(samples: &[f32]) -> f64 {
    samples.len() as f64 / SAMPLE_RATE_HZ * 1000.0
}

enum AudioCmd {
    Start(std::sync::mpsc::Sender<Result<()>>),
    Stop(std::sync::mpsc::Sender<Option<Vec<f32>>>),
    Snapshot(std::sync::mpsc::Sender<Option<Vec<f32>>>),
    Cancel,
    Shutdown,
}

/// Real capture, confined to its own thread because `cpal::Stream` is `!Send`.
pub struct HostAudioSource {
    tx: std::sync::mpsc::Sender<AudioCmd>,
    recording: Arc<AtomicBool>,
    /// Whether the capture thread currently holds an input device open.
    input_open: Arc<AtomicBool>,
    pre_roll_ms: u32,
    diagnostics: Arc<Mutex<CaptureDiagnostics>>,
    meter: LevelMeter,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl HostAudioSource {
    /// Spawn the capture thread and open the device handle on it.
    ///
    /// # Errors
    ///
    /// Propagates a device-open failure from the capture thread.
    pub fn new(config: crate::config::AudioConfig) -> Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<AudioCmd>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<LevelMeter>>();
        let recording = Arc::new(AtomicBool::new(false));
        let flag = recording.clone();
        let input_open = Arc::new(AtomicBool::new(false));
        let open_flag = input_open.clone();
        let pre_roll_ms = config.pre_roll_ms;
        let diagnostics = Arc::new(Mutex::new(CaptureDiagnostics::default()));
        let diagnostics_for_thread = diagnostics.clone();

        let thread = std::thread::Builder::new()
            .name("dictate-audio".into())
            .spawn(move || {
                let mut capture = match AudioCapture::new(config) {
                    Ok(c) => {
                        open_flag.store(c.is_armed(), Ordering::Release);
                        let _ = ready_tx.send(Ok(c.level_meter()));
                        c
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                // `AudioCapture::stop` is async (it awaits the trailing-capture
                // delay), so this thread needs a runtime of its own to drive it.
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("audio thread runtime failed to start: {e}");
                        return;
                    }
                };
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        AudioCmd::Start(reply) => {
                            let r = capture.start();
                            flag.store(r.is_ok(), Ordering::Release);
                            open_flag.store(capture.is_armed(), Ordering::Release);
                            let _ = reply.send(r);
                        }
                        AudioCmd::Stop(reply) => {
                            let samples = rt.block_on(capture.stop());
                            if let Ok(mut last) = diagnostics_for_thread.lock() {
                                *last = capture.diagnostics();
                            }
                            flag.store(false, Ordering::Release);
                            open_flag.store(capture.is_armed(), Ordering::Release);
                            let _ = reply.send(samples);
                        }
                        AudioCmd::Snapshot(reply) => {
                            let samples = capture.is_recording().then(|| capture.snapshot());
                            let _ = reply.send(samples);
                        }
                        AudioCmd::Cancel => {
                            capture.cancel();
                            flag.store(false, Ordering::Release);
                            open_flag.store(capture.is_armed(), Ordering::Release);
                        }
                        AudioCmd::Shutdown => break,
                    }
                }
                capture.cancel();
            })?;

        let meter = ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("audio capture thread died during startup"))??;

        Ok(Self {
            tx,
            recording,
            input_open,
            pre_roll_ms,
            diagnostics,
            meter,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// Await a reply from the capture thread without blocking the runtime.
    async fn ask<T: Send + 'static>(
        &self,
        make: impl FnOnce(std::sync::mpsc::Sender<T>) -> AudioCmd,
        on_dead: T,
    ) -> T {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel::<T>();
        if self.tx.send(make(reply_tx)).is_err() {
            return on_dead;
        }
        tokio::task::spawn_blocking(move || reply_rx.recv().ok())
            .await
            .ok()
            .flatten()
            .unwrap_or(on_dead)
    }
}

impl Drop for HostAudioSource {
    fn drop(&mut self) {
        let _ = self.tx.send(AudioCmd::Shutdown);
        if let Some(handle) = self.thread.lock().ok().and_then(|mut t| t.take()) {
            let _ = handle.join();
        }
    }
}

impl AudioSource for HostAudioSource {
    fn start(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.ask(
                AudioCmd::Start,
                Err(anyhow::anyhow!("audio thread is gone")),
            )
            .await
        })
    }

    fn stop(&self) -> BoxFuture<'_, Option<Vec<f32>>> {
        Box::pin(async move { self.ask(AudioCmd::Stop, None).await })
    }

    fn cancel(&self) {
        let _ = self.tx.send(AudioCmd::Cancel);
        self.recording.store(false, Ordering::Release);
    }

    fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Acquire)
    }

    fn snapshot(&self) -> BoxFuture<'_, Option<Vec<f32>>> {
        Box::pin(async move { self.ask(AudioCmd::Snapshot, None).await })
    }

    fn diagnostics(&self) -> CaptureDiagnostics {
        self.diagnostics
            .lock()
            .map(|last| *last)
            .unwrap_or_default()
    }

    fn input_status(&self) -> Option<dictate_proto::AudioStatus> {
        Some(dictate_proto::AudioStatus {
            capture_enabled: true,
            input_open: self.input_open.load(Ordering::Acquire),
            pre_roll_ms: Some(self.pre_roll_ms),
        })
    }

    fn level(&self) -> Option<Level> {
        self.meter.take()
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Result of the LLM formatting pass.
#[derive(Debug, Clone)]
pub struct Formatted {
    /// The text to carry forward. On failure this is the input, unchanged —
    /// the pass fails open.
    pub text: String,
    /// Whether the pass altered anything.
    pub changed: bool,
    /// Why it failed, if it did. Present *and* `text` unchanged means the
    /// stage burned time and fell back, which the timings must report as
    /// `Failed`, not `Ran`.
    pub error: Option<String>,
    /// Wall time the pass cost, including a failed attempt.
    pub duration_s: f64,
}

/// Whether the formatting pass will actually run for a given input.
///
/// # Why this is separate from `format`
///
/// A formatter applies its own skip rules (disabled, too short, too long,
/// model unavailable) and returns the input unchanged when it declines —
/// which is byte-for-byte
/// indistinguishable from having run and found nothing to fix. Reporting that
/// as `Ran{0.3}` would put a fabricated number in the latency table S12 and
/// the ≤1.0s budget are measured from, and would hide the fact that the LLM
/// pass is being skipped for most short utterances.
///
/// So the decision is lifted out: the pipeline asks first, records
/// `Skipped{reason}` without calling `format`, and only reports `Ran`/`Failed`
/// for a pass that genuinely executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatPlan {
    /// The pass will run.
    Run,
    /// The pass declines, for this reason.
    Skip(dictate_proto::SkipReason),
}

/// The LLM formatting pass.
///
/// It runs after the deterministic chain and the router, and only for
/// `Route::Type` (the pipeline skips it for every other route with
/// `route_not_eligible`). `text` is the rules output with protected spans
/// restored; the pipeline verifies the result against those spans and rejects
/// any output that drops, duplicates or alters one, so an implementation
/// cannot corrupt a slash command or path even if its model tries to.
pub trait Formatter: Send + Sync + 'static {
    /// Format `text`, failing open.
    fn format<'a>(&'a self, text: &'a str, ctx: &'a FormatContext) -> BoxFuture<'a, Formatted>;

    /// Whether this input would be formatted, and if not, why not.
    fn plan(&self, text: &str, ctx: &FormatContext) -> FormatPlan;

    /// The pass's *observed* health, for `get_status`. A formatter that fails
    /// open is otherwise invisible; `None` means this one does not report.
    fn status(&self) -> Option<dictate_proto::FormatterStatus> {
        None
    }

    /// Check the formatter's backend once, at startup, and record the answer —
    /// so a missing model is reported before the first dictation instead of
    /// being discovered by nobody. Never fails; the default does nothing.
    fn probe(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// A session toward an app of `category` just started recording: load
    /// whatever the pass needs while the user is still speaking. Must never
    /// block; the default does nothing.
    fn warm_up(&self, _category: &dictate_proto::AppCategory, _tone: &dictate_proto::Tone) {}
}

// ---------------------------------------------------------------------------
// Injection
// ---------------------------------------------------------------------------

/// Text injection into the focused application.
///
/// The call is async because a Wayland portal can return an outstanding
/// consent request. X11 is currently immediate, but keeping this boundary
/// async avoids making a future consent flow a breaking redesign.
pub trait TextInjector: Send + Sync + 'static {
    /// Inject `text`, reporting what actually happened. An absent per-call
    /// policy inherits the global default; overrides still obey backend caps.
    fn inject(
        &self,
        text: &str,
        policy: Option<dictate_inject::InjectionPolicy>,
    ) -> BoxFuture<'_, InjectionOutcome>;

    /// Capture only a destination identity, without reading titles/clipboard.
    /// Test and portal adapters avoid host X11 I/O by default.
    fn capture_destination(&self) -> Option<u32> {
        None
    }

    /// Additive stop-time binding seam. Host backends validate at delivery;
    /// synthetic/portal implementations may override their own window identity.
    fn inject_bound(
        &self,
        text: &str,
        policy: Option<dictate_inject::InjectionPolicy>,
        _destination: Option<Option<u32>>,
    ) -> BoxFuture<'_, InjectionOutcome> {
        self.inject(text, policy)
    }

    /// Whether injection is possible here at all. `false` on a headless host,
    /// and the reason the outcome becomes `Unavailable` rather than `Failed`.
    fn is_available(&self) -> bool;
}

/// Clipboard-paste injection behind the trait.
pub struct HostInjector {
    inner: dictate_inject::X11Injector,
    enabled: bool,
}

impl HostInjector {
    /// Build from output config.
    #[must_use]
    pub fn new(config: &dictate_inject::OutputConfig) -> Self {
        Self {
            inner: dictate_inject::X11Injector::new(config),
            enabled: config.auto_type,
        }
    }
}

impl TextInjector for HostInjector {
    fn inject(
        &self,
        text: &str,
        policy: Option<dictate_inject::InjectionPolicy>,
    ) -> BoxFuture<'_, InjectionOutcome> {
        let policy = policy.unwrap_or_else(|| self.inner.default_policy());
        let inner = self.inner.clone();
        let text = text.to_owned();
        Box::pin(async move {
            if !self.enabled {
                return InjectionOutcome::Skipped {
                    reason: dictate_proto::SkipReason::Disabled,
                };
            }
            tokio::task::spawn_blocking(move || inner.inject_blocking(&text, policy))
                .await
                .unwrap_or_else(|e| InjectionOutcome::Failed {
                    error: dictate_proto::ProtoError::new(
                        dictate_proto::ErrorCode::InjectionFailed,
                        format!("injection task failed: {e}"),
                    ),
                })
        })
    }

    fn capture_destination(&self) -> Option<u32> {
        dictate_inject::focused_window()
    }

    fn inject_bound(
        &self,
        text: &str,
        policy: Option<dictate_inject::InjectionPolicy>,
        destination: Option<Option<u32>>,
    ) -> BoxFuture<'_, InjectionOutcome> {
        let policy = policy.unwrap_or_else(|| self.inner.default_policy());
        let inner = self.inner.clone();
        let text = text.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                inner.inject_bound_blocking(&text, policy, destination)
            })
            .await
            .unwrap_or_else(|e| InjectionOutcome::Failed {
                error: dictate_proto::ProtoError::new(
                    dictate_proto::ErrorCode::InjectionFailed,
                    format!("injection task failed: {e}"),
                ),
            })
        })
    }

    fn is_available(&self) -> bool {
        use dictate_inject::Injector as _;
        let caps = self.inner.capabilities();
        self.enabled && (caps.clipboard_save_restore || caps.direct_typing)
    }
}

// ---------------------------------------------------------------------------
// Desktop side effects
// ---------------------------------------------------------------------------

/// A user-visible status change worth a desktop notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// Capture started.
    Recording,
    /// The pipeline is running.
    Transcribing,
    /// A route is executing against the named model.
    Processing(String),
    /// Nothing was said.
    NoSpeech,
    /// The session was cancelled.
    Cancelled,
    /// A timer was set.
    TimerSet(String),
    /// Injection failed or became clipboard-only; never overwrite clipboard.
    InjectionFailed(String),
    /// Something failed.
    Error(String),
    /// Capture was silent for long enough to indicate a muted microphone.
    MicrophoneMuted,
    /// Clear any transient status.
    Clear,
}

/// Desktop notifications.
pub trait StatusNotifier: Send + Sync + 'static {
    /// Show (or clear) a status.
    fn notify(&self, notice: Notice);
}

/// `notify-rust` behind the trait.
pub struct DesktopNotifier {
    inner: Mutex<crate::notify::Notifier>,
}

impl DesktopNotifier {
    /// Build from notification config.
    #[must_use]
    pub fn new(config: &crate::config::NotificationConfig) -> Self {
        Self {
            inner: Mutex::new(crate::notify::Notifier::new(config)),
        }
    }
}

impl StatusNotifier for DesktopNotifier {
    fn notify(&self, notice: Notice) {
        let Ok(mut n) = self.inner.lock() else {
            return;
        };
        match notice {
            Notice::Recording => n.recording(),
            Notice::Transcribing => n.transcribing(),
            Notice::Processing(model) => n.processing(&model),
            Notice::NoSpeech => n.no_speech(),
            Notice::Cancelled => n.cancelled(),
            Notice::TimerSet(msg) => n.timer_set(&msg),
            Notice::Error(msg) => n.error(&msg),
            Notice::InjectionFailed(msg) => n.injection_failed(&msg),
            Notice::MicrophoneMuted => n.microphone_muted(),
            Notice::Clear => n.clear_status(),
        }
    }
}

/// Pause and resume whatever the user was listening to.
pub trait MediaController: Send + Sync + 'static {
    /// Pause if something is playing, remembering that we did.
    fn pause_if_playing(&self) -> bool;
    /// Resume only if we were the one who paused.
    fn resume_if_needed(&self) -> bool;
}

/// `playerctl` behind the trait, with the same state file today's daemon uses.
#[derive(Debug, Default)]
pub struct PlayerctlMedia;

impl MediaController for PlayerctlMedia {
    fn pause_if_playing(&self) -> bool {
        let is_playing = std::process::Command::new("playerctl")
            .arg("status")
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .eq_ignore_ascii_case("Playing")
            })
            .unwrap_or(false);

        let state_path = crate::config::media_state_path();
        if is_playing {
            let paused = std::process::Command::new("playerctl")
                .arg("pause")
                .status()
                .is_ok_and(|status| status.success());
            if paused {
                let _ = std::fs::write(&state_path, "playing");
                tracing::info!("Media paused");
            }
            paused
        } else if state_path.exists() {
            // Clean up a state file left by a previous crash.
            let _ = std::fs::remove_file(&state_path);
            false
        } else {
            false
        }
    }

    fn resume_if_needed(&self) -> bool {
        let state_path = crate::config::media_state_path();
        if state_path.exists() {
            let _ = std::fs::remove_file(&state_path);
            let resumed = std::process::Command::new("playerctl")
                .arg("play")
                .status()
                .is_ok_and(|status| status.success());
            if resumed {
                tracing::info!("Media resumed");
            }
            resumed
        } else {
            false
        }
    }
}

/// Short auditory feedback is deliberately separate from desktop status
/// notifications. A missing output device must never make dictation fail.
pub trait AudioFeedback: Send + Sync + 'static {
    /// Queue a cue and return whether an earcon was enabled and queued.
    fn play(&self, cue: EarconCue) -> bool;
}

/// Rodio-backed production earcons.
#[derive(Debug, Clone)]
pub struct HostEarcons {
    inner: EarconPlayer,
}

impl HostEarcons {
    #[must_use]
    pub fn new(config: dictate_audio::EarconConfig) -> Self {
        Self {
            inner: EarconPlayer::new(config),
        }
    }
}

impl AudioFeedback for HostEarcons {
    fn play(&self, cue: EarconCue) -> bool {
        self.inner.play(cue)
    }
}

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// Deterministic stand-ins for every port above.
///
/// Compiled into the library under the `test-support` feature so integration
/// tests in `dictated` — a different crate — can drive the real engine over a
/// real socket with none of the hardware.
#[cfg(any(test, feature = "test-support"))]
pub mod mock {
    use super::*;
    use dictate_proto::InjectMethod;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Notify;

    /// Capture that yields a fixed buffer, with no device and no delay.
    #[derive(Debug)]
    pub struct MockAudio {
        samples: Vec<f32>,
        recording: AtomicBool,
        /// Number of times `cancel` was called — proves the capture stream was
        /// torn down rather than left dangling.
        pub cancels: AtomicUsize,
        stop_delay: std::time::Duration,
        snapshot_available: bool,
        level: Option<Level>,
    }

    impl MockAudio {
        /// Capture that returns `secs` seconds of silence.
        #[must_use]
        pub fn with_seconds(secs: f64) -> Self {
            Self {
                samples: vec![0.0; (secs * SAMPLE_RATE_HZ) as usize],
                recording: AtomicBool::new(false),
                cancels: AtomicUsize::new(0),
                stop_delay: std::time::Duration::ZERO,
                snapshot_available: true,
                level: None,
            }
        }

        /// Capture that returns nothing, exercising the no-audio path.
        #[must_use]
        pub fn empty() -> Self {
            Self {
                samples: Vec::new(),
                recording: AtomicBool::new(false),
                cancels: AtomicUsize::new(0),
                stop_delay: std::time::Duration::ZERO,
                snapshot_available: true,
                level: None,
            }
        }

        /// Make `stop` take `delay`, standing in for the trailing-capture wait
        /// so a test can cancel during it.
        #[must_use]
        pub fn with_stop_delay(mut self, delay: std::time::Duration) -> Self {
            self.stop_delay = delay;
            self
        }

        /// Report this input level on every read while recording.
        #[must_use]
        pub fn with_level(mut self, rms: f32) -> Self {
            self.level = Some(Level { rms, peak: rms });
            self
        }

        /// Simulate a capture backend that cannot provide live snapshots.
        #[must_use]
        pub fn without_snapshots(mut self) -> Self {
            self.snapshot_available = false;
            self
        }

        /// How many times capture was torn down.
        pub fn cancel_count(&self) -> usize {
            self.cancels.load(Ordering::Acquire)
        }
    }

    impl AudioSource for MockAudio {
        fn start(&self) -> BoxFuture<'_, Result<()>> {
            self.recording.store(true, Ordering::Release);
            Box::pin(async { Ok(()) })
        }

        fn stop(&self) -> BoxFuture<'_, Option<Vec<f32>>> {
            Box::pin(async move {
                if self.stop_delay > std::time::Duration::ZERO {
                    tokio::time::sleep(self.stop_delay).await;
                }
                self.recording.store(false, Ordering::Release);
                if self.samples.is_empty() {
                    None
                } else {
                    Some(self.samples.clone())
                }
            })
        }

        fn cancel(&self) {
            self.cancels.fetch_add(1, Ordering::AcqRel);
            self.recording.store(false, Ordering::Release);
        }

        fn is_recording(&self) -> bool {
            self.recording.load(Ordering::Acquire)
        }

        fn snapshot(&self) -> BoxFuture<'_, Option<Vec<f32>>> {
            Box::pin(async move {
                (self.snapshot_available && self.recording.load(Ordering::Acquire))
                    .then(|| self.samples.clone())
            })
        }

        fn level(&self) -> Option<Level> {
            self.level
                .filter(|_| self.recording.load(Ordering::Acquire))
        }
    }

    /// Deterministic VAD double for pipeline tests.
    pub struct MockVad {
        decision: GateDecision,
        auto_stop: bool,
        gates: AtomicUsize,
    }

    impl MockVad {
        #[must_use]
        pub fn returning(decision: GateDecision) -> Self {
            Self {
                decision,
                auto_stop: false,
                gates: AtomicUsize::new(0),
            }
        }

        #[must_use]
        pub fn auto_stopping(mut self) -> Self {
            self.auto_stop = true;
            self
        }

        #[must_use]
        pub fn gate_count(&self) -> usize {
            self.gates.load(Ordering::Acquire)
        }
    }

    impl VoiceActivityGate for MockVad {
        fn gate(&self, _samples: &[f32]) -> Result<GateDecision> {
            self.gates.fetch_add(1, Ordering::AcqRel);
            Ok(self.decision.clone())
        }

        fn trailing_silence_tracker(&self) -> Result<Box<dyn TrailingSilenceTracker>> {
            Ok(Box::new(MockTrailingSilence {
                auto_stop: self.auto_stop,
            }))
        }

        fn enabled(&self) -> bool {
            true
        }

        fn poll_interval_ms(&self) -> u32 {
            1
        }
    }

    struct MockTrailingSilence {
        auto_stop: bool,
    }

    impl TrailingSilenceTracker for MockTrailingSilence {
        fn observe_snapshot(&mut self, _samples: &[f32]) -> Result<bool> {
            Ok(self.auto_stop)
        }
    }

    /// Recognizer that returns fixed text, optionally slowly, optionally failing.
    pub struct MockStt {
        text: Option<String>,
        delay: std::time::Duration,
        fail: Option<String>,
        gate: Option<Arc<Gate>>,
    }

    impl MockStt {
        /// Always transcribes to `text`.
        #[must_use]
        pub fn returning(text: &str) -> Self {
            Self {
                text: Some(text.to_string()),
                delay: std::time::Duration::ZERO,
                fail: None,
                gate: None,
            }
        }

        /// Always reports "no speech".
        #[must_use]
        pub fn silent() -> Self {
            Self {
                text: None,
                delay: std::time::Duration::ZERO,
                fail: None,
                gate: None,
            }
        }

        /// Always fails with `msg`.
        #[must_use]
        pub fn failing(msg: &str) -> Self {
            Self {
                text: None,
                delay: std::time::Duration::ZERO,
                fail: Some(msg.to_string()),
                gate: None,
            }
        }

        /// Take `delay` before answering, so a test can cancel mid-`Transcribing`.
        #[must_use]
        pub fn with_delay(mut self, delay: std::time::Duration) -> Self {
            self.delay = delay;
            self
        }

        /// Block until the returned gate is opened — a deterministic
        /// alternative to sleeping when a test needs to be *inside* this stage.
        #[must_use]
        pub fn with_gate(mut self, gate: Arc<Gate>) -> Self {
            self.gate = Some(gate);
            self
        }
    }

    impl SttProvider for MockStt {
        fn transcribe<'a>(
            &'a self,
            _samples: &'a [f32],
            _request: SttRequest,
        ) -> BoxFuture<'a, Result<Option<Transcription>>> {
            Box::pin(async move {
                if let Some(gate) = &self.gate {
                    gate.mark_entered();
                    gate.opened().await;
                }
                if self.delay > std::time::Duration::ZERO {
                    tokio::time::sleep(self.delay).await;
                }
                if let Some(msg) = &self.fail {
                    anyhow::bail!("{msg}");
                }
                Ok(self.text.clone().map(|text| Transcription {
                    text,
                    language: Some("en".into()),
                    timings: dictate_stt::SttTimings::default(),
                }))
            })
        }

        fn model(&self) -> ModelInfo {
            ModelInfo {
                name: "mock".into(),
                loaded: true,
                backend: Some("mock".into()),
            }
        }
    }

    /// A rendezvous a test uses to hold a pipeline stage open.
    ///
    /// Arrival is recorded as a **counter**, not a notification: a test that
    /// has not parked yet would miss a `notify_waiters`, and the resulting
    /// flake would look exactly like the concurrency bug the test exists to
    /// catch. Polling a monotonic counter cannot miss an arrival that already
    /// happened.
    #[derive(Debug, Default)]
    pub struct Gate {
        open: AtomicBool,
        notify: Notify,
        entered: AtomicUsize,
    }

    impl Gate {
        /// A closed gate.
        #[must_use]
        pub fn closed() -> Arc<Self> {
            Arc::new(Self::default())
        }

        /// Let the waiting stage through.
        pub fn open(&self) {
            self.open.store(true, Ordering::Release);
            self.notify.notify_waiters();
        }

        /// Whether the gate has been opened.
        #[must_use]
        pub fn is_open(&self) -> bool {
            self.open.load(Ordering::Acquire)
        }

        /// Record that a stage has reached this gate.
        pub fn mark_entered(&self) {
            self.entered.fetch_add(1, Ordering::AcqRel);
        }

        /// How many times a stage has reached this gate.
        #[must_use]
        pub fn entry_count(&self) -> usize {
            self.entered.load(Ordering::Acquire)
        }

        /// Wait until a stage has reached this gate.
        pub async fn wait_entered(&self) {
            while self.entry_count() == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        }

        async fn opened(&self) {
            loop {
                let waiter = self.notify.notified();
                if self.open.load(Ordering::Acquire) {
                    return;
                }
                waiter.await;
                if self.open.load(Ordering::Acquire) {
                    return;
                }
            }
        }
    }

    /// Formatter that passes text through, optionally appending or rewriting
    /// so a test can prove the stage ran (or misbehaved).
    pub struct MockFormatter {
        enabled: bool,
        suffix: Option<String>,
        delay: std::time::Duration,
        error: Option<String>,
        rewrite: Option<fn(&str) -> String>,
        calls: AtomicUsize,
        last_context: Mutex<Option<FormatContext>>,
    }

    impl Default for MockFormatter {
        fn default() -> Self {
            Self {
                enabled: true,
                suffix: None,
                delay: std::time::Duration::ZERO,
                error: None,
                rewrite: None,
                calls: AtomicUsize::new(0),
                last_context: Mutex::new(None),
            }
        }
    }

    impl MockFormatter {
        /// A formatter that is configured off.
        #[must_use]
        pub fn disabled() -> Self {
            Self {
                enabled: false,
                ..Self::default()
            }
        }

        /// Append `suffix` so the test can see the stage ran.
        #[must_use]
        pub fn appending(suffix: &str) -> Self {
            Self {
                suffix: Some(suffix.to_string()),
                ..Self::default()
            }
        }

        /// Burn `delay` and then fail open, the Ollama-is-hanging case.
        #[must_use]
        pub fn failing_after(delay: std::time::Duration, error: &str) -> Self {
            Self {
                delay,
                error: Some(error.to_string()),
                ..Self::default()
            }
        }

        /// Take `delay` before answering.
        #[must_use]
        pub fn with_delay(mut self, delay: std::time::Duration) -> Self {
            self.delay = delay;
            self
        }

        /// Return `rewrite(text)` — e.g. the production failure where the
        /// model stripped the `/` from `/research_codebase`.
        #[must_use]
        pub fn rewriting(rewrite: fn(&str) -> String) -> Self {
            Self {
                rewrite: Some(rewrite),
                ..Self::default()
            }
        }

        /// How many times `format` was called.
        #[must_use]
        pub fn calls(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }

        /// The context the last `format` call received.
        #[must_use]
        pub fn last_context(&self) -> Option<FormatContext> {
            self.last_context.lock().ok().and_then(|c| c.clone())
        }
    }

    impl Formatter for MockFormatter {
        fn format<'a>(&'a self, text: &'a str, ctx: &'a FormatContext) -> BoxFuture<'a, Formatted> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::AcqRel);
                if let Ok(mut last) = self.last_context.lock() {
                    *last = Some(ctx.clone());
                }
                let start = std::time::Instant::now();
                if self.delay > std::time::Duration::ZERO {
                    tokio::time::sleep(self.delay).await;
                }
                let out = match (&self.error, &self.suffix, self.rewrite) {
                    // Fail open: the input survives, and the time is still spent.
                    (Some(_), _, _) => text.to_string(),
                    (None, _, Some(rewrite)) => rewrite(text),
                    (None, Some(s), None) => format!("{text}{s}"),
                    (None, None, None) => text.to_string(),
                };
                Formatted {
                    changed: out != text,
                    text: out,
                    error: self.error.clone(),
                    duration_s: start.elapsed().as_secs_f64(),
                }
            })
        }

        fn plan(&self, _text: &str, _ctx: &FormatContext) -> FormatPlan {
            if self.enabled {
                FormatPlan::Run
            } else {
                FormatPlan::Skip(dictate_proto::SkipReason::Disabled)
            }
        }
    }

    /// Injector that records what it was asked to type.
    #[derive(Debug)]
    pub struct MockInjector {
        injected: Arc<Mutex<Vec<String>>>,
        policies: Arc<Mutex<Vec<Option<dictate_inject::InjectionPolicy>>>>,
        available: bool,
        availability_gate: Option<Arc<Gate>>,
        gate: Option<Arc<Gate>>,
        fail: bool,
    }

    impl Default for MockInjector {
        fn default() -> Self {
            Self {
                injected: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
                available: true,
                availability_gate: None,
                gate: None,
                fail: false,
            }
        }
    }

    impl MockInjector {
        /// Per-call policies observed by the double.
        pub fn policies(&self) -> Vec<Option<dictate_inject::InjectionPolicy>> {
            self.policies
                .lock()
                .expect("mock injector poisoned")
                .clone()
        }
        /// A working injector.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// An injector on a host that cannot inject.
        #[must_use]
        pub fn unavailable() -> Self {
            Self {
                available: false,
                ..Self::default()
            }
        }

        /// An injector whose paste fails.
        #[must_use]
        pub fn failing() -> Self {
            Self {
                fail: true,
                ..Self::default()
            }
        }

        /// Block inside `inject` until the gate opens.
        ///
        /// This holds the pipeline **past** the commit point, which is how a
        /// test drives the "cancel arrived too late" branch deterministically
        /// instead of racing a real 50ms clipboard paste.
        #[must_use]
        pub fn with_gate(mut self, gate: Arc<Gate>) -> Self {
            self.gate = Some(gate);
            self
        }

        /// Block inside `is_available` until the gate opens.
        ///
        /// The availability probe runs in the `injecting` stage but **before**
        /// the commit point, so this holds the pipeline exactly where a cancel
        /// must still succeed with nothing typed. It is not a contrivance: a
        /// Wayland portal probe genuinely involves IPC and can block.
        #[must_use]
        pub fn with_availability_gate(mut self, gate: Arc<Gate>) -> Self {
            self.availability_gate = Some(gate);
            self
        }

        /// Everything this injector was asked to type, in order.
        ///
        /// The assertion that matters for cancellation: after a cancelled
        /// session this must be empty.
        pub fn injected(&self) -> Vec<String> {
            self.injected
                .lock()
                .expect("mock injector poisoned")
                .clone()
        }
    }

    impl TextInjector for MockInjector {
        fn inject(
            &self,
            text: &str,
            policy: Option<dictate_inject::InjectionPolicy>,
        ) -> BoxFuture<'_, InjectionOutcome> {
            let text = text.to_string();
            self.policies
                .lock()
                .expect("mock injector poisoned")
                .push(policy);
            let injected = self.injected.clone();
            let gate = self.gate.clone();
            let available = self.available;
            let fail = self.fail;
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    if let Some(gate) = &gate {
                        gate.mark_entered();
                        while !gate.is_open() {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                    }
                    if !available {
                        return InjectionOutcome::Unavailable {
                            backend: "none".into(),
                            reason: "mock injector is unavailable".into(),
                        };
                    }
                    if fail {
                        return InjectionOutcome::Failed {
                            error: dictate_proto::ProtoError::new(
                                dictate_proto::ErrorCode::InjectionFailed,
                                "mock injection failure",
                            ),
                        };
                    }
                    injected
                        .lock()
                        .expect("mock injector poisoned")
                        .push(text.to_string());
                    InjectionOutcome::Injected {
                        method: if policy == Some(dictate_inject::InjectionPolicy::Type) {
                            InjectMethod::Keystroke
                        } else {
                            InjectMethod::Paste
                        },
                        chars: text.trim().chars().count() as u32,
                    }
                })
                .await
                .unwrap_or_else(|e| InjectionOutcome::Failed {
                    error: dictate_proto::ProtoError::new(
                        dictate_proto::ErrorCode::InjectionFailed,
                        format!("mock injection task failed: {e}"),
                    ),
                })
            })
        }

        fn is_available(&self) -> bool {
            if let Some(gate) = &self.availability_gate {
                gate.mark_entered();
                while !gate.is_open() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            self.available
        }
    }

    /// Notifier that records notices instead of talking to the desktop.
    #[derive(Debug, Default)]
    pub struct RecordingNotifier {
        notices: Mutex<Vec<Notice>>,
    }

    impl RecordingNotifier {
        /// Every notice raised, in order.
        pub fn notices(&self) -> Vec<Notice> {
            self.notices.lock().expect("notifier poisoned").clone()
        }
    }

    impl StatusNotifier for RecordingNotifier {
        fn notify(&self, notice: Notice) {
            self.notices.lock().expect("notifier poisoned").push(notice);
        }
    }

    /// Media controller that does nothing — CI has no player and no `playerctl`.
    #[derive(Debug, Default)]
    pub struct NullMedia;

    impl MediaController for NullMedia {
        fn pause_if_playing(&self) -> bool {
            false
        }
        fn resume_if_needed(&self) -> bool {
            false
        }
    }

    /// Earcons disabled for deterministic tests.
    #[derive(Debug, Default)]
    pub struct NullEarcons;

    impl AudioFeedback for NullEarcons {
        fn play(&self, _: EarconCue) -> bool {
            false
        }
    }

    /// Deterministic side effects for daemon event-stream tests.
    #[derive(Debug, Default)]
    pub struct ActiveMedia;

    impl MediaController for ActiveMedia {
        fn pause_if_playing(&self) -> bool {
            true
        }
        fn resume_if_needed(&self) -> bool {
            true
        }
    }

    #[derive(Debug, Default)]
    pub struct RecordingEarcons;

    impl AudioFeedback for RecordingEarcons {
        fn play(&self, _: EarconCue) -> bool {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_ms_matches_the_16khz_sample_rate() {
        assert!((audio_ms(&vec![0.0; 16_000]) - 1000.0).abs() < f64::EPSILON);
        assert!((audio_ms(&[]) - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn mock_audio_reports_and_clears_recording() {
        use mock::MockAudio;
        let a = MockAudio::with_seconds(1.0);
        assert!(!a.is_recording());
        a.start().await.unwrap();
        assert!(a.is_recording());
        let samples = a.stop().await.expect("one second of samples");
        assert_eq!(samples.len(), 16_000);
        assert!(!a.is_recording());
    }

    #[tokio::test]
    async fn mock_audio_cancel_is_counted_and_idempotent() {
        use mock::MockAudio;
        let a = MockAudio::with_seconds(1.0);
        a.start().await.unwrap();
        a.cancel();
        a.cancel();
        assert_eq!(a.cancel_count(), 2);
        assert!(!a.is_recording());
    }

    #[tokio::test]
    async fn mock_formatter_fails_open_preserving_input_and_time() {
        use mock::MockFormatter;
        let f = MockFormatter::failing_after(std::time::Duration::from_millis(5), "ollama down");
        let out = f.format("hello", &FormatContext::default()).await;
        assert_eq!(out.text, "hello", "a failed format pass must not lose text");
        assert!(out.error.is_some());
        assert!(out.duration_s > 0.0, "burned time must still be reported");
    }

    #[tokio::test]
    async fn mock_injector_records_what_it_typed() {
        use mock::MockInjector;
        let i = MockInjector::new();
        assert!(i.injected().is_empty());
        let outcome = i.inject("hello there", None).await;
        assert!(matches!(outcome, InjectionOutcome::Injected { .. }));
        assert_eq!(i.injected(), vec!["hello there".to_string()]);
    }
}
