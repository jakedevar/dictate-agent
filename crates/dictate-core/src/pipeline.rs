//! One dictation session, start to terminal state.
//!
//! This is today's `stop_recording_and_process` re-expressed as a cancellable
//! task. The stages, their order, and their behavior are unchanged — capture,
//! transcribe, correct, route, dispatch, log — because this slice is about the
//! control plane, not about changing what dictation does. What is new:
//!
//! 1. **A single owner.** One task owns a session from `Recording` to its
//!    terminal state, so there is exactly one teardown path
//!    ([`Pipeline::finish`]) and no way to reach a terminal state without
//!    releasing the microphone and resuming the user's music.
//! 2. **Checkpoints, not polling.** Every stage boundary goes through
//!    [`SessionHandle::advance_checked`], and every `await` that could outlast
//!    a user's patience is raced against
//!    [`CancelToken::cancelled`](crate::cancel::CancelToken::cancelled). You
//!    cannot enter a stage without passing the cancellation check, because the
//!    check is the thing that moves you there.
//! 3. **A commit point.** Injection is the one irreversible stage, so it is
//!    entered through [`CancelToken::enter_commit`], which atomically takes the
//!    session out of the cancellable set. A cancel that loses that race is told
//!    `TooLate` rather than being allowed to report success over text that is
//!    already in the user's editor.
//! 4. **Honest timings.** Each stage records `Ran` / `Skipped{reason}` /
//!    `Failed{ms}` / `NotReported` rather than a number that might be a lie.
//!    See [`Stages`].
//!
//! [`CancelToken::enter_commit`]: crate::cancel::CancelToken::enter_commit

use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Re-exported: the type of [`Pipeline::text_chain`].
pub use dictate_fmt::TextChain;
use dictate_fmt::{FormatContext, SpanKind, TextDoc};
use dictate_history::history::Interaction;
use dictate_history::HistoryStore;
use dictate_proto::{
    AudioActivity, DictationMode, ErrorCode, Event, FinalText, InjectionOutcome, ProtoError, Route,
    SkipReason, StageTiming, StageTimings, State, Transcript,
};
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use crate::cancel::CancelToken;
use crate::local_executor::LocalExecutor;
use crate::ports::{
    audio_ms, AudioFeedback, AudioSource, FormatPlan, Formatter, GateDecision, MediaController,
    Notice, StatusNotifier, SttProvider, SttRequest, TextInjector, VoiceActivityGate,
};
use crate::router::{self, RouteType};
use crate::session::SessionHandle;
use crate::timer::TimerExecutor;
use dictate_audio::EarconCue;

/// Map the router's internal enum onto the wire enum.
///
/// Two enums rather than one because `dictate-proto` must not depend on
/// `dictate-core` (it is the leaf every consumer shares) and the router
/// predates the protocol.
#[must_use]
pub fn route_to_proto(route: RouteType) -> Route {
    match route {
        RouteType::Type => Route::Type,
        RouteType::Local => Route::Local,
        RouteType::Timer => Route::Timer,
        RouteType::Edit => Route::Edit,
        RouteType::Command => Route::Command,
        RouteType::Note => Route::Note,
    }
}

/// Per-stage timing accumulator.
///
/// Starts as all-[`StageTiming::NotReported`] and is filled in as stages
/// execute, so a stage the pipeline never reached reads as "no data" rather
/// than as a fabricated zero. `total_ms` is measured across the whole session
/// rather than summed from the stages — the gap between the two is scheduling
/// and queueing time that belongs to no stage, and it is the signal S12 uses.
#[derive(Debug)]
pub struct Stages {
    timings: StageTimings,
    started: Instant,
}

impl Default for Stages {
    fn default() -> Self {
        Self::new()
    }
}

impl Stages {
    /// Begin accounting.
    #[must_use]
    pub fn new() -> Self {
        Self {
            // Every stage — including the deterministic text chain, which
            // reports its own measured `fmt_rules` since S20 — starts as
            // `not_reported` and is filled in only when it actually runs.
            timings: StageTimings::default(),
            started: Instant::now(),
        }
    }

    /// Elapsed wall time since the session's pipeline began.
    #[must_use]
    pub fn elapsed_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    /// Finalize, stamping the measured (not derived) total.
    #[must_use]
    pub fn finish(mut self, audio_ms_value: Option<f64>) -> StageTimings {
        self.timings.total_ms = Some(self.elapsed_ms());
        self.timings.audio_ms = audio_ms_value;
        self.timings
    }
}

/// Time one stage and record the outcome.
struct StageClock(Instant);

impl StageClock {
    fn start() -> Self {
        Self(Instant::now())
    }

    fn ms(&self) -> f64 {
        self.0.elapsed().as_secs_f64() * 1000.0
    }

    fn ran(&self) -> StageTiming {
        StageTiming::ran(self.ms())
    }

    fn failed(&self, error: impl Into<String>) -> StageTiming {
        StageTiming::Failed {
            ms: self.ms(),
            error: Some(error.into()),
        }
    }
}

/// Stop-time X11 identity shared between the engine and the pipeline.
pub type StopDestination = Arc<Mutex<Option<Option<u32>>>>;

/// How a session's options resolved against the caller's capabilities.
#[derive(Debug, Clone)]
pub struct ResolvedOptions {
    /// Whether to inject the result into the focused app. `false` means the
    /// caller takes delivery instead, which is a success, not a skip.
    pub inject: bool,
    /// Shared with the engine's explicit stop action. Outer None means not yet
    /// stopped, inner None means no X11 focus could be verified. Uploads omit it.
    pub stop_destination: Option<StopDestination>,
    /// A route forced by the caller, bypassing the router.
    pub forced_route: Option<Route>,
    /// Routes this caller may invoke at all. Deny-by-default per S01 item 2:
    /// an empty list permits nothing.
    pub allowed_routes: Vec<Route>,
    /// Suppress persistence of transcript text for this session.
    pub privacy: bool,
    /// Disable recognizer bias, replacements and formatter vocabulary together.
    pub use_dictionary: bool,
    /// `SessionOptions.format_llm`: `Some(false)` skips the LLM pass for this
    /// session; `None`/`Some(true)` leave the decision to the formatter's own
    /// skip rules and the app profile.
    pub format_llm: Option<bool>,
    /// Caller-supplied app ID; it resolves without querying host focus/titles.
    pub app: Option<String>,
    /// Host focus may only be read for trusted local live capture sessions.
    /// Upload callers must set this false before resolving context.
    pub capture_context: bool,
    /// Start snapshot, replaced by the stop-time app decision for live sessions.
    pub context: Option<dictate_proto::AppContext>,
    /// Resolved overrides consumed by formatting and injection.
    pub profile: dictate_proto::ResolvedProfile,
}

impl Default for ResolvedOptions {
    fn default() -> Self {
        Self {
            inject: true,
            stop_destination: None,
            forced_route: None,
            allowed_routes: Route::known().to_vec(),
            privacy: false,
            use_dictionary: true,
            format_llm: None,
            app: None,
            capture_context: true,
            context: None,
            profile: dictate_proto::ResolvedProfile::default(),
        }
    }
}

/// What a finished session produced.
#[derive(Debug, Clone)]
pub struct PipelineOutcome {
    /// The terminal state reached: `done`, `error`, or `cancelled`.
    pub state: State,
    /// The transcript, when one was produced.
    pub transcript: Option<Transcript>,
    /// Why the session failed, when it did.
    pub error: Option<ProtoError>,
}

/// Everything a session needs to run, shared by `Arc` across session tasks.
pub struct Pipeline {
    /// Bounded focus provider and validated profiles.
    pub context: Arc<dictate_context::ContextEngine>,
    /// Microphone.
    pub audio: Arc<dyn AudioSource>,
    /// Recognizer.
    pub stt: Arc<dyn SttProvider>,
    /// Shared immutable matcher snapshots and control-plane dictionary store.
    pub dictionary: Option<Arc<dictate_dict::Dictionary>>,
    /// Silero gate/trim and hands-free trailing-silence tracker.
    pub vad: Arc<dyn VoiceActivityGate>,
    /// Deterministic text chain (S20): scrub, protect, corrections, rules.
    pub text_chain: Arc<TextChain>,
    /// LLM formatting pass.
    pub formatter: Arc<dyn Formatter>,
    /// Text injection.
    pub injector: Arc<dyn TextInjector>,
    /// Desktop notifications.
    pub notifier: Arc<dyn StatusNotifier>,
    /// Media pause/resume.
    pub media: Arc<dyn MediaController>,
    /// Optional non-blocking start/stop/cancel/error earcons.
    pub earcons: Arc<dyn AudioFeedback>,
    /// Interaction log.
    pub history: Arc<Mutex<HistoryStore>>,
    /// Ollama executor for the `local` route.
    pub local: Arc<LocalExecutor>,
    /// Selection rewrite executor (S25).
    pub editor: Arc<crate::edit_executor::EditExecutor>,
    /// `systemd-run` executor for the `timer` route.
    pub timer: Arc<TimerExecutor>,
    /// Model name used by the `local` route, for notifications.
    pub local_model: String,
}

/// Wire profile policy mapped to the existing capability-aware injector.
fn context_policy(
    profile: &dictate_proto::ResolvedProfile,
) -> Option<dictate_inject::InjectionPolicy> {
    use dictate_inject::InjectionPolicy;
    use dictate_proto::ContextInjection;
    match profile.inject.as_ref() {
        Some(ContextInjection::Paste) => Some(InjectionPolicy::Paste),
        Some(ContextInjection::Type) => Some(InjectionPolicy::Type),
        Some(ContextInjection::Off | ContextInjection::Unknown(_)) => Some(InjectionPolicy::Off),
        None => None,
    }
}

/// The result of one stage that may have been cut short.
enum Step<T> {
    Continue(T),
    Cancelled,
}

impl Pipeline {
    /// Resolve at acceptance; live sessions refresh when recording stops.
    /// Headless/upload consumers call this with capture_context = false.
    pub fn resolve_context(&self, options: &mut ResolvedOptions) {
        options.profile = self
            .context
            .resolve(options.app.as_deref(), options.capture_context);
        options.context = options.profile.context.clone();
    }
    /// Run one session to a terminal state.
    ///
    /// A live session begins in `Recording` (the engine has already opened the
    /// device) and waits on `stop`. An upload (see
    /// [`SessionHandle::is_upload`]) skips the recording phase and every side
    /// effect around it, and takes its samples from the handle. Returns the terminal state so the engine
    /// can clear its slot; every event a client needs has already been
    /// published by then.
    pub async fn run(
        self: Arc<Self>,
        handle: SessionHandle,
        stop: Arc<Notify>,
        mut opts: ResolvedOptions,
    ) -> PipelineOutcome {
        let mut stages = Stages::new();
        let token = handle.token().clone();
        // An upload transcribes audio it was handed: there is no microphone,
        // so no "recording" notice, media pause, or earcon around it. The
        // engine has already stored the stop permit, so the wait below passes
        // straight through (still losing to a cancel).
        let upload = handle.is_upload();

        handle.publish(Event::ContextResolved {
            session_id: handle.id().clone(),
            context: opts.context.clone(),
        });
        // Load the formatting model while the user is still speaking.
        self.formatter.warm_up(
            &opts
                .context
                .as_ref()
                .map(|c| c.category.clone())
                .unwrap_or_default(),
            &opts.profile.tone,
        );

        if !upload {
            self.notifier.notify(Notice::Recording);
        }
        self.earcon(&handle, EarconCue::Start, AudioActivity::EarconStart);
        // Best-effort and slow (a `playerctl` subprocess), so it runs off the
        // engine's thread and is not allowed to delay the state machine.
        if !upload {
            let media = self.media.clone();
            let paused = tokio::task::spawn_blocking(move || media.pause_if_playing())
                .await
                .unwrap_or(false);
            if paused {
                handle.publish(Event::AudioActivity {
                    session_id: handle.id().clone(),
                    activity: AudioActivity::MediaPaused,
                    at_ms: None,
                });
            }
        }

        // --- Recording: wait for stop, or for a cancel from any actor -------
        // `biased` puts cancellation first: if both a stop and a cancel are
        // pending, the session must end cancelled rather than transcribe audio
        // the user asked to throw away.
        tokio::select! {
            biased;
            () = token.cancelled() => {
                return self.finish(&handle, stages, None, Outcome::cancelled()).await;
            }
            () = stop.notified() => {}
            () = self.auto_stop(&handle, &token), if matches!(handle.mode(), DictationMode::OneShot | DictationMode::WakeWord) => {}
            () = self.meter(&handle) => {}
        }

        if !upload {
            if let Some(binding) = &opts.stop_destination {
                let mut slot = binding.lock().expect("stop destination poisoned");
                if slot.is_none() {
                    *slot = Some(self.injector.capture_destination());
                }
            }
            // Explicit stops were captured synchronously by the engine; VAD
            // stops arrive here immediately after observing trailing silence.
            let profile = handle.stop_profile().unwrap_or_else(|| {
                self.context
                    .resolve(opts.app.as_deref(), opts.capture_context)
            });
            if profile.context.as_ref().map(|c| &c.app) != opts.context.as_ref().map(|c| &c.app) {
                opts.context = profile.context.clone();
                opts.profile = profile;
                handle.publish(Event::ContextResolved {
                    session_id: handle.id().clone(),
                    context: opts.context.clone(),
                });
            }
        }

        // EDIT consumes the same stop-time identity as ordinary dictation.
        // A failed capture stays failed; recognition must never recapture focus.
        let edit_destination = if !upload
            && opts.inject
            && opts.capture_context
            && context_policy(&opts.profile) != Some(dictate_inject::InjectionPolicy::Off)
        {
            opts.stop_destination
                .as_ref()
                .and_then(|binding| *binding.lock().expect("stop destination poisoned"))
                .flatten()
        } else {
            None
        };

        let mut interaction = {
            let store = self.history.lock().expect("history mutex poisoned");
            store.begin()
        };
        interaction.no_store = opts.privacy;
        interaction.app_context = if opts.privacy {
            None
        } else {
            opts.context.as_ref().map(|c| c.app.clone())
        };
        interaction.stt_model = Some(self.stt.model().name);

        // --- Capture flush --------------------------------------------------
        self.notifier.notify(Notice::Transcribing);
        if handle.advance_checked(State::Transcribing).is_err() {
            return self
                .finish(&handle, stages, Some(interaction), Outcome::cancelled())
                .await;
        }

        let clock = StageClock::start();
        let samples = match self.race(&token, self.take_audio(&handle)).await {
            Step::Cancelled => {
                stages.timings.capture = clock.failed("cancelled during capture flush");
                return self
                    .finish(&handle, stages, Some(interaction), Outcome::cancelled())
                    .await;
            }
            Step::Continue(s) => s,
        };
        stages.timings.capture = handle
            .supplied_audio()
            .map_or_else(|| clock.ran(), |a| StageTiming::ran(a.prepare_ms));

        self.earcon(&handle, EarconCue::Stop, AudioActivity::EarconStop);
        let diagnostics = if upload {
            dictate_audio::CaptureDiagnostics::default()
        } else {
            self.audio.diagnostics()
        };
        if diagnostics.recovered_device {
            handle.publish(Event::AudioActivity {
                session_id: handle.id().clone(),
                activity: AudioActivity::DeviceRecovered,
                at_ms: None,
            });
        }
        if diagnostics.mic_mute_suspected {
            self.notifier.notify(Notice::MicrophoneMuted);
            handle.publish(Event::AudioActivity {
                session_id: handle.id().clone(),
                activity: AudioActivity::MicrophoneMuted,
                at_ms: None,
            });
        }

        let Some(samples) = samples else {
            warn!("No audio captured");
            self.notifier.notify(Notice::NoSpeech);
            stages.timings.vad = StageTiming::skipped(SkipReason::NoSpeechDetected);
            stages.timings.stt = StageTiming::skipped(SkipReason::NoSpeechDetected);
            stages.timings.inject = StageTiming::skipped(SkipReason::NoSpeechDetected);
            return self
                .finish(
                    &handle,
                    stages,
                    Some(interaction),
                    Outcome::done(empty_transcript(SkipReason::NoSpeechDetected)),
                )
                .await;
        };

        let audio_len_ms = audio_ms(&samples);
        interaction.audio_duration_s = Some(audio_len_ms / 1000.0);

        // --- Voice activity gate and silence trim -------------------------
        // This timing deliberately starts *after* recording has stopped. The
        // one-shot monitor below spans user speaking time and is not latency.
        let clock = StageClock::start();
        let gated = match self
            .race(&token, {
                let vad = self.vad.clone();
                let captured = samples.clone();
                async move { tokio::task::spawn_blocking(move || vad.gate(&captured)).await }
            })
            .await
        {
            Step::Cancelled => {
                stages.timings.vad = clock.failed("cancelled during voice activity detection");
                return self
                    .finish(
                        &handle,
                        stages,
                        Some(interaction),
                        Outcome::cancelled_after(audio_len_ms),
                    )
                    .await;
            }
            Step::Continue(Ok(Ok(decision))) => {
                stages.timings.vad = clock.ran();
                decision
            }
            Step::Continue(Ok(Err(e))) => {
                stages.timings.vad = clock.failed(e.to_string());
                let error = ProtoError::new(ErrorCode::Internal, format!("VAD failed: {e}"));
                return self
                    .finish(
                        &handle,
                        stages,
                        Some(interaction),
                        Outcome::error(error, audio_len_ms),
                    )
                    .await;
            }
            Step::Continue(Err(e)) => {
                stages.timings.vad = clock.failed("VAD worker failed");
                let error = ProtoError::new(ErrorCode::Internal, format!("VAD worker failed: {e}"));
                return self
                    .finish(
                        &handle,
                        stages,
                        Some(interaction),
                        Outcome::error(error, audio_len_ms),
                    )
                    .await;
            }
        };
        let samples = match gated {
            GateDecision::NoSpeech => {
                info!(
                    audio_ms = audio_len_ms,
                    "VAD gated no-speech capture; skipping STT"
                );
                self.notifier.notify(Notice::NoSpeech);
                stages.timings.stt = StageTiming::skipped(SkipReason::NoSpeechDetected);
                stages.timings.inject = StageTiming::skipped(SkipReason::NoSpeechDetected);
                return self
                    .finish(
                        &handle,
                        stages,
                        Some(interaction),
                        Outcome::done(empty_transcript(SkipReason::NoSpeechDetected))
                            .with_audio_ms(audio_len_ms),
                    )
                    .await;
            }
            GateDecision::Speech {
                samples,
                leading_trimmed_ms,
                trailing_trimmed_ms,
            } => {
                info!(
                    trimmed_leading_ms = leading_trimmed_ms,
                    trimmed_trailing_ms = trailing_trimmed_ms,
                    retained_ms = audio_ms(&samples),
                    "VAD retained speech span"
                );
                samples
            }
        };

        // --- S22 dictionary: bounded per-session vocabulary bias ------------
        // Scope by the S23 stop-time context for live sessions (or the
        // caller-supplied app id); `None` applies global entries only.
        let dictionary_app = opts.context.clone();
        let stt_request = SttRequest {
            initial_prompt: self.dictionary.as_ref().and_then(|dictionary| {
                dictionary.initial_prompt(dictionary_app.as_ref(), opts.use_dictionary)
            }),
            ..SttRequest::default()
        };

        // --- Speech to text -------------------------------------------------
        let clock = StageClock::start();
        let transcribed = match self
            .race(&token, self.stt.transcribe(&samples, stt_request))
            .await
        {
            Step::Cancelled => {
                stages.timings.stt = clock.failed("cancelled during transcription");
                return self
                    .finish(
                        &handle,
                        stages,
                        Some(interaction),
                        Outcome::cancelled_after(audio_len_ms),
                    )
                    .await;
            }
            Step::Continue(r) => r,
        };

        let transcribed = match transcribed {
            Ok(t) => {
                stages.timings.stt = clock.ran();
                t
            }
            Err(e) => {
                error!("Transcription failed: {}", e);
                stages.timings.stt = clock.failed(e.to_string());
                self.notifier.notify(Notice::Clear);
                self.notifier
                    .notify(Notice::Error(format!("Transcription failed: {e}")));
                interaction.error_summary = Some(format!("Transcription failed: {e}"));
                let err = ProtoError::new(ErrorCode::SttFailed, e.to_string());
                return self
                    .finish(
                        &handle,
                        stages,
                        Some(interaction),
                        Outcome::error(err, audio_len_ms),
                    )
                    .await;
            }
        };
        interaction.transcription_duration_s =
            stages.timings.stt.elapsed_ms().map(|ms| ms / 1000.0);

        let Some(transcribed) = transcribed else {
            info!("No speech detected");
            self.notifier.notify(Notice::NoSpeech);
            stages.timings.inject = StageTiming::skipped(SkipReason::NoSpeechDetected);
            return self
                .finish(
                    &handle,
                    stages,
                    Some(interaction),
                    Outcome::done(empty_transcript(SkipReason::NoSpeechDetected))
                        .with_audio_ms(audio_len_ms),
                )
                .await;
        };

        let raw_text = transcribed.text.clone();
        interaction.raw_transcription = Some(raw_text.clone());
        interaction.corrected_transcription = Some(raw_text.clone());
        // One privacy decision for everything below: the session asked for it,
        // or the store is globally private (an unreadable store counts as
        // private). Logs reach the journal, so in private sessions they carry
        // no transcript text at any stage.
        let private = opts.privacy
            || self
                .history
                .lock()
                .map(|store| store.is_privacy_mode())
                .unwrap_or(true);
        if !private {
            info!("Transcribed: \"{}\"", raw_text);
        }

        // --- Formatting -----------------------------------------------------
        if handle.advance_checked(State::Formatting).is_err() {
            return self
                .finish(
                    &handle,
                    stages,
                    Some(interaction),
                    Outcome::cancelled_after(audio_len_ms),
                )
                .await;
        }

        // Deterministic chain (S20). Synchronous and sub-millisecond, so it
        // runs inline: there is no await here for a cancel to race.
        let mut ctx = FormatContext {
            // The immutable S23 snapshot (focus at session start, or the app a
            // caller named); `None` is the normal headless/remote case.
            app: opts.context.clone(),
            tone: opts.profile.tone.clone(),
            // Provisional inside the chain: routing runs on its output.
            route: opts.forced_route.clone().unwrap_or_default(),
            language: transcribed.language.clone(),
            vocabulary: match &self.dictionary {
                Some(dictionary) if opts.use_dictionary => {
                    dictionary.vocabulary(opts.context.as_ref())
                }
                _ => Vec::new(),
            },
            use_dictionary: opts.use_dictionary,
            persist: !private,
            spoken_punctuation: opts.profile.spoken_punctuation,
            spoken_line_breaks: opts.profile.spoken_line_breaks,
            format_llm: opts.format_llm,
            // Filled below from the chain's output.
            protected: Vec::new(),
            // Snippet variables may read the clipboard only for a session with
            // a user at this desktop, never for an uploaded recording.
            host_variables: !upload,
        };
        let (doc, rules_text) = if self.text_chain.is_enabled() {
            let clock = StageClock::start();
            let run = self.text_chain.run(&raw_text, &ctx);
            let rules_text = run.doc.restore();
            stages.timings.fmt_rules = clock.ran();
            debug!(stages = %run.timings, "text chain");
            (run.doc, rules_text)
        } else {
            stages.timings.fmt_rules = StageTiming::skipped(SkipReason::Disabled);
            // No rewriting rules, but the LLM guard below still needs to know
            // which spans it must not let the model touch.
            (TextDoc::protected(&raw_text), raw_text.clone())
        };
        // Logs reach the journal: in privacy mode they carry no text.
        if rules_text != raw_text && !private {
            info!("Formatted: \"{}\"", rules_text);
        }
        interaction.grammar_input = Some(rules_text.clone());
        interaction.corrected_transcription = Some(rules_text.clone());
        // The LLM pass masks exactly the spans the chain protected, so a model
        // never sees a slash command, path or dictionary term it could rewrite.
        ctx.protected = doc.protected_byte_ranges();

        // --- Route ----------------------------------------------------------
        // On the rules output and before the LLM pass, so a trigger word
        // ("timer", "easy", "edit:") is never at the mercy of a model, and a
        // non-`type` route never pays for one.
        // A snippet expansion is stored text, not something the user said, so
        // one that opens the utterance must not be read as a route trigger: an
        // expansion starting "timer …" or "edit: …" would otherwise run a
        // timer or rewrite the selection instead of being typed.
        let opens_with_snippet = doc
            .working_text()
            .trim_start()
            .chars()
            .next()
            .and_then(|c| doc.span_for(c))
            .is_some_and(|s| s.kind == SpanKind::Snippet);
        let routed = if opens_with_snippet {
            router::RouteResult {
                route: RouteType::Type,
                model: String::new(),
                text: rules_text.clone(),
                confidence: 1.0,
            }
        } else {
            router::route(&rules_text)
        };
        let resolved_route = opts
            .forced_route
            .clone()
            .unwrap_or_else(|| route_to_proto(routed.route.clone()));
        ctx.route = resolved_route.clone();
        if private {
            info!("Routed to {:?}", resolved_route);
        } else {
            info!("Routed to {:?}: \"{}\"", resolved_route, routed.text);
        }

        interaction.route_type = Some(resolved_route.as_str().to_string());
        interaction.route_model = Some(routed.model.clone());
        interaction.route_confidence = Some(routed.confidence);

        // --- LLM formatting pass (`type` prose only) ------------------------
        let plan = if resolved_route != Route::Type {
            FormatPlan::Skip(SkipReason::RouteNotEligible)
        } else if opts.format_llm == Some(false)
            || (opts.profile.llm_format == Some(false) && opts.format_llm != Some(true))
        {
            // The caller, or the app profile unless the caller insisted, asked
            // for rules-only text.
            FormatPlan::Skip(SkipReason::Disabled)
        } else {
            self.formatter.plan(&rules_text, &ctx)
        };
        let text = match plan {
            FormatPlan::Skip(reason) => {
                stages.timings.fmt_llm = StageTiming::skipped(reason);
                rules_text.clone()
            }
            FormatPlan::Run => {
                let clock = StageClock::start();
                match self
                    .race(&token, self.formatter.format(&rules_text, &ctx))
                    .await
                {
                    Step::Cancelled => {
                        stages.timings.fmt_llm = clock.failed("cancelled during formatting");
                        return self
                            .finish(
                                &handle,
                                stages,
                                Some(interaction),
                                Outcome::cancelled_after(audio_len_ms),
                            )
                            .await;
                    }
                    Step::Continue(formatted) => {
                        // Protected spans must come back byte-for-byte. An
                        // output that drops, duplicates or edits one (the
                        // production model stripped `/research_codebase` to
                        // `research_codebase`) is rejected, and the session
                        // keeps the rules output.
                        let rejected = match &formatted.error {
                            Some(_) => None,
                            None => doc
                                .verify_output(&formatted.text)
                                .err()
                                .map(|v| format!("formatter output rejected: {v}")),
                        };
                        let error = formatted.error.clone().or_else(|| rejected.clone());
                        // A pass that burned time and then fell back is
                        // `Failed`, not `Ran` — that time is in the user's
                        // latency budget and must not vanish from accounting.
                        stages.timings.fmt_llm = match &error {
                            Some(e) => clock.failed(e.clone()),
                            None => clock.ran(),
                        };
                        interaction.grammar_output = Some(formatted.text.clone());
                        interaction.grammar_changed = formatted.changed && error.is_none();
                        interaction.grammar_error = error.clone();
                        interaction.grammar_duration_s = Some(formatted.duration_s);
                        if let Some(reason) = &rejected {
                            if private {
                                warn!(
                                    "formatter altered a protected span; keeping the rules output"
                                );
                            } else {
                                warn!("{reason}; keeping the rules output");
                            }
                        } else if formatted.changed && error.is_none() && !private {
                            info!(
                                "Grammar corrected: \"{}\" → \"{}\"",
                                rules_text, formatted.text
                            );
                        }
                        if error.is_some() {
                            rules_text.clone()
                        } else {
                            formatted.text
                        }
                    }
                }
            }
        };
        interaction.corrected_transcription = Some(text.clone());

        // Deny-by-default route gating (S01 item 2, per the Epic-lead overrule).
        // The router can land on `timer`, which runs `systemd-run` on this
        // host; a caller that was not granted that route must not reach it by
        // saying the right word.
        if !opts.allowed_routes.contains(&resolved_route) {
            let msg = format!(
                "route '{}' is not permitted for this connection",
                resolved_route.as_str()
            );
            warn!("{msg}");
            stages.timings.inject = StageTiming::skipped(SkipReason::NotPermitted);
            interaction.error_summary = Some(msg.clone());
            let err = ProtoError::new(ErrorCode::Forbidden, msg);
            return self
                .finish(
                    &handle,
                    stages,
                    Some(interaction),
                    Outcome::error(err, audio_len_ms),
                )
                .await;
        }

        // --- Dispatch and inject --------------------------------------------
        if handle.advance_checked(State::Injecting).is_err() {
            return self
                .finish(
                    &handle,
                    stages,
                    Some(interaction),
                    Outcome::cancelled_after(audio_len_ms),
                )
                .await;
        }

        let dispatch = self
            .dispatch(
                &token,
                &mut stages,
                &mut interaction,
                &opts,
                resolved_route.clone(),
                &routed.text,
                &text,
                edit_destination,
            )
            .await;

        let (final_text, injection) = match dispatch {
            Step::Cancelled => {
                return self
                    .finish(
                        &handle,
                        stages,
                        Some(interaction),
                        Outcome::cancelled_after(audio_len_ms),
                    )
                    .await;
            }
            Step::Continue(v) => v,
        };

        if let InjectionOutcome::Failed { error } = &injection {
            interaction.error_summary = Some(error.message.clone());
            if resolved_route != Route::Edit {
                self.notifier
                    .notify(Notice::InjectionFailed(error.message.clone()));
            }
        }

        let word_count = final_text.split_whitespace().count() as u32;
        let transcript = Transcript {
            text: FinalText(final_text),
            raw_text: if opts.privacy {
                None
            } else {
                interaction.raw_transcription.clone()
            },
            route: resolved_route,
            timings: StageTimings::default(),
            injection,
            word_count: Some(word_count),
            model: Some(self.stt.model().name),
        };

        interaction.completed = true;
        self.finish(
            &handle,
            stages,
            Some(interaction),
            Outcome::done(transcript).with_audio_ms(audio_len_ms),
        )
        .await
    }

    /// The session's samples: the microphone flush for a live session, or the
    /// audio an upload was handed. `None` means there was nothing.
    async fn take_audio(&self, handle: &SessionHandle) -> Option<Vec<f32>> {
        match handle.supplied_audio() {
            Some(audio) if audio.samples.is_empty() => None,
            Some(audio) => Some(audio.samples.clone()),
            None => self.audio.stop().await,
        }
    }

    /// Race a stage future against cancellation.
    ///
    /// Losing the race drops the stage future, which is why every port is
    /// written to be drop-safe: the audio thread completes its own work and
    /// the recognizer's buffers are freed with the future.
    async fn race<T>(
        &self,
        token: &CancelToken,
        fut: impl std::future::Future<Output = T>,
    ) -> Step<T> {
        tokio::select! {
            biased;
            () = token.cancelled() => Step::Cancelled,
            v = fut => Step::Continue(v),
        }
    }

    /// Publish the input level ~30 times a second while recording, for HUD
    /// meters (S32). Never completes: it only ever loses the recording
    /// `select!`. A source that reports no level publishes nothing.
    async fn meter(&self, handle: &SessionHandle) {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(33));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Some(level) = self.audio.level() {
                handle.publish(Event::AudioLevel {
                    session_id: handle.id().clone(),
                    rms: level.rms,
                    peak: Some(level.peak),
                    at_ms: None,
                });
            }
        }
    }

    /// Wait for end-of-speech in a hands-free session. Its work is excluded
    /// from the final VAD timing because it occurs while the person speaks.
    async fn auto_stop(&self, handle: &SessionHandle, token: &CancelToken) {
        let mut tracker = match self.vad.trailing_silence_tracker() {
            Ok(tracker) => tracker,
            Err(error) => {
                warn!(%error, session = %handle.id().as_str(), "one-shot VAD unavailable; explicit stop required");
                std::future::pending::<()>().await;
                return;
            }
        };
        let interval = std::time::Duration::from_millis(u64::from(self.vad.poll_interval_ms()));
        loop {
            tokio::select! {
                biased;
                () = token.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
            let Some(snapshot) = self.audio.snapshot().await else {
                warn!(session = %handle.id().as_str(), "one-shot audio snapshots unavailable; explicit stop required");
                std::future::pending::<()>().await;
                return;
            };
            match tracker.observe_snapshot(&snapshot) {
                Ok(true) => {
                    info!(session = %handle.id().as_str(), "one-shot VAD detected trailing silence");
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, session = %handle.id().as_str(), "one-shot VAD tracker failed; explicit stop required");
                    std::future::pending::<()>().await;
                }
            }
        }
    }

    /// Execute the resolved route and inject its output.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch(
        &self,
        token: &CancelToken,
        stages: &mut Stages,
        interaction: &mut Interaction,
        opts: &ResolvedOptions,
        route: Route,
        route_text: &str,
        full_text: &str,
        edit_destination: Option<u32>,
    ) -> Step<(String, InjectionOutcome)> {
        match route {
            Route::Type => {
                let outcome = self.inject(token, stages, opts, full_text).await;
                match outcome {
                    Step::Cancelled => Step::Cancelled,
                    Step::Continue(o) => {
                        interaction.output_typed = o.did_inject();
                        if o.did_inject() {
                            interaction.output_char_count = Some(full_text.len());
                        }
                        Step::Continue((full_text.to_string(), o))
                    }
                }
            }
            Route::Edit => {
                let instruction = if router::route(full_text).route == RouteType::Edit {
                    route_text
                } else {
                    full_text
                };
                self.edit(
                    token,
                    stages,
                    interaction,
                    opts,
                    instruction,
                    edit_destination,
                )
                .await
            }
            Route::Note => {
                // As with edit: the router already stripped the trigger, but a
                // caller-forced route sees the whole utterance.
                let body = if router::route(full_text).route == RouteType::Note {
                    route_text
                } else {
                    full_text
                };
                self.note(token, stages, interaction, opts, body)
            }
            Route::Local => {
                self.notifier
                    .notify(Notice::Processing(self.local_model.clone()));
                interaction.prompt_sent = Some(route_text.to_string());

                let started = Instant::now();
                let result = match self.race(token, self.local.execute(route_text, None)).await {
                    Step::Cancelled => return Step::Cancelled,
                    Step::Continue(r) => r,
                };
                interaction.execution_duration_s = Some(started.elapsed().as_secs_f64());
                interaction.execution_success = Some(result.success);
                // The model the ladder actually resolved to, which is not
                // `[local].model` when that one is missing; `None` when no
                // model could be asked at all.
                interaction.execution_model = result.model.clone();

                if !result.success {
                    let msg = result.error.clone().unwrap_or_default();
                    interaction.execution_error = result.error.clone();
                    self.notifier.notify(Notice::Error(msg.clone()));
                    stages.timings.inject = StageTiming::skipped(SkipReason::DependencyUnavailable);
                    return Step::Continue((
                        String::new(),
                        InjectionOutcome::Failed {
                            error: ProtoError::new(ErrorCode::Internal, msg),
                        },
                    ));
                }

                interaction.response_text = Some(result.response.clone());
                match self.inject(token, stages, opts, &result.response).await {
                    Step::Cancelled => Step::Cancelled,
                    Step::Continue(o) => {
                        interaction.output_typed = o.did_inject();
                        if o.did_inject() {
                            interaction.output_char_count = Some(result.response.len());
                        }
                        Step::Continue((result.response, o))
                    }
                }
            }
            Route::Timer => {
                let timer = self.timer.clone();
                let text = route_text.to_string();
                let result = match self
                    .race(token, async move {
                        tokio::task::spawn_blocking(move || timer.execute(&text)).await
                    })
                    .await
                {
                    Step::Cancelled => return Step::Cancelled,
                    Step::Continue(Ok(r)) => r,
                    Step::Continue(Err(e)) => {
                        error!("timer executor panicked: {e}");
                        stages.timings.inject = StageTiming::skipped(SkipReason::RouteNotEligible);
                        return Step::Continue((
                            String::new(),
                            InjectionOutcome::Failed {
                                error: ProtoError::new(
                                    ErrorCode::Internal,
                                    "timer executor panicked",
                                ),
                            },
                        ));
                    }
                };
                interaction.execution_success = Some(result.success);
                if result.success {
                    interaction.response_text = Some(result.response.clone());
                    self.notifier
                        .notify(Notice::TimerSet(result.response.clone()));
                } else {
                    interaction.execution_error = result.error.clone();
                    self.notifier
                        .notify(Notice::Error(result.error.clone().unwrap_or_default()));
                }
                // A timer never types anything: the stage did not run because
                // this route is not eligible for it, which is a skip and not a
                // zero-cost run.
                stages.timings.inject = StageTiming::skipped(SkipReason::RouteNotEligible);
                Step::Continue((
                    result.response,
                    InjectionOutcome::Skipped {
                        reason: SkipReason::RouteNotEligible,
                    },
                ))
            }
            other => {
                // `command` is named by the protocol but has no
                // implementation yet. Reporting the stage as `not_supported`
                // is the honest answer; inventing a plausible-looking success
                // would corrupt the parity measurements.
                let msg = format!("route '{}' is not implemented", other.as_str());
                warn!("{msg}");
                self.notifier.notify(Notice::Error(msg.clone()));
                interaction.error_summary = Some(msg.clone());
                stages.timings.inject = StageTiming::skipped(SkipReason::NotSupported);
                Step::Continue((
                    String::new(),
                    InjectionOutcome::Skipped {
                        reason: SkipReason::NotSupported,
                    },
                ))
            }
        }
    }

    /// Append the utterance to the scratchpad (S35). Nothing is typed.
    ///
    /// The write is irreversible, so it takes the commit point first: a
    /// cancelled session provably stores nothing, and a `cancel` that arrives
    /// once the note is being written is answered `TooLate`.
    fn note(
        &self,
        token: &CancelToken,
        stages: &mut Stages,
        interaction: &mut Interaction,
        opts: &ResolvedOptions,
        body: &str,
    ) -> Step<(String, InjectionOutcome)> {
        stages.timings.inject = StageTiming::skipped(SkipReason::RouteNotEligible);
        let refuse =
            |this: &Self, interaction: &mut Interaction, reason: SkipReason, message: &str| {
                interaction.execution_success = Some(false);
                interaction.error_summary = Some(message.to_string());
                this.notifier.notify(Notice::Error(message.to_string()));
                Step::Continue((String::new(), InjectionOutcome::Skipped { reason }))
            };
        if body.trim_matches(|c: char| !c.is_alphanumeric()).is_empty() {
            return refuse(
                self,
                interaction,
                SkipReason::NoSpeechDetected,
                "there was nothing to note",
            );
        }
        // The session's own privacy request, on top of the store's global one.
        if opts.privacy {
            return refuse(
                self,
                interaction,
                SkipReason::NotPermitted,
                "privacy mode is on, so the note was not saved",
            );
        }
        let Some(guard) = token.enter_commit() else {
            return Step::Cancelled;
        };
        let saved = match self.history.lock() {
            Ok(store) => store.add_note(body),
            Err(_) => {
                drop(guard);
                return refuse(
                    self,
                    interaction,
                    SkipReason::DependencyUnavailable,
                    "the note store is unavailable",
                );
            }
        };
        drop(guard);
        match saved {
            Ok(note) => {
                interaction.execution_success = Some(true);
                interaction.output_char_count = Some(note.text.chars().count());
                self.notifier.notify(Notice::NoteSaved(note.text.clone()));
                Step::Continue((
                    note.text,
                    InjectionOutcome::Skipped {
                        reason: SkipReason::RouteNotEligible,
                    },
                ))
            }
            Err(error) => {
                let reason = match error {
                    dictate_history::NoteError::Private | dictate_history::NoteError::Disabled => {
                        SkipReason::NotPermitted
                    }
                    dictate_history::NoteError::Empty => SkipReason::NoSpeechDetected,
                    dictate_history::NoteError::Storage(_) => SkipReason::DependencyUnavailable,
                };
                interaction.execution_error = Some(error.to_string());
                refuse(self, interaction, reason, &error.to_string())
            }
        }
    }

    async fn edit(
        &self,
        token: &CancelToken,
        stages: &mut Stages,
        interaction: &mut Interaction,
        opts: &ResolvedOptions,
        instruction: &str,
        destination: Option<u32>,
    ) -> Step<(String, InjectionOutcome)> {
        // Ctrl+C is a copy operation in editors, but interrupts shell programs.
        // Never send it to a terminal, or read selections for an upload.
        if !opts.inject
            || !opts.capture_context
            || destination.is_none()
            || context_policy(&opts.profile)
                .is_some_and(|policy| policy != dictate_inject::InjectionPolicy::Paste)
            || opts
                .context
                .as_ref()
                .is_some_and(|c| c.category == dictate_proto::AppCategory::Terminal)
        {
            stages.timings.inject = StageTiming::skipped(SkipReason::NotPermitted);
            interaction.execution_success = Some(false);
            return Step::Continue((
                String::new(),
                InjectionOutcome::Skipped {
                    reason: SkipReason::NotPermitted,
                },
            ));
        }
        if instruction
            .trim()
            .trim_matches(['.', ',', ':', ';', '!', '?'])
            .trim()
            .is_empty()
        {
            return self.edit_failure(stages, interaction, "edit instruction is empty".into());
        }
        let selection = match self
            .race(
                token,
                self.injector
                    .capture_selection(destination.expect("checked above")),
            )
            .await
        {
            Step::Cancelled => return Step::Cancelled,
            Step::Continue(Ok(selection)) => selection,
            Step::Continue(Err(error)) => {
                return self.edit_failure(stages, interaction, error.to_string())
            }
        };
        self.notifier
            .notify(Notice::Processing(self.local_model.clone()));
        let clock = StageClock::start();
        let replacement = match self
            .race(token, self.editor.execute(instruction, &selection.text))
            .await
        {
            Step::Cancelled => {
                stages.timings.fmt_llm = clock.failed("cancelled during edit");
                return Step::Cancelled;
            }
            Step::Continue(Ok(text)) => {
                stages.timings.fmt_llm = clock.ran();
                text
            }
            Step::Continue(Err(error)) => {
                stages.timings.fmt_llm = clock.failed(error.to_string());
                return self.edit_failure(stages, interaction, error.to_string());
            }
        };
        if self.editor.preview_only {
            if token.is_cancelled() {
                return Step::Cancelled;
            }
            // Preview can contain selected text; privacy mode suppresses it.
            if !opts.privacy
                && !self
                    .history
                    .lock()
                    .map(|store| store.is_privacy_mode())
                    .unwrap_or(true)
            {
                self.notifier
                    .notify(Notice::EditPreview(replacement.clone()));
            }
            interaction.execution_success = Some(true);
            stages.timings.inject = StageTiming::skipped(SkipReason::Disabled);
            return Step::Continue((
                replacement,
                InjectionOutcome::Skipped {
                    reason: SkipReason::Disabled,
                },
            ));
        }
        let Some(guard) = token.enter_commit() else {
            return Step::Cancelled;
        };
        let clock = StageClock::start();
        let outcome = self
            .injector
            .replace_selection(selection, replacement.clone())
            .await;
        drop(guard);
        stages.timings.inject = match &outcome {
            InjectionOutcome::Failed { error } => clock.failed(error.message.clone()),
            _ => clock.ran(),
        };
        interaction.execution_success = Some(outcome.did_inject());
        interaction.output_typed = outcome.did_inject();
        if let InjectionOutcome::Failed { error } = &outcome {
            interaction.execution_error = Some(error.message.clone());
            self.notifier
                .notify(Notice::EditError(error.message.clone()));
        }
        if outcome.did_inject() {
            interaction.output_char_count = Some(replacement.chars().count());
            interaction.response_text = Some(replacement.clone());
        }
        Step::Continue((replacement, outcome))
    }

    fn edit_failure(
        &self,
        stages: &mut Stages,
        interaction: &mut Interaction,
        message: String,
    ) -> Step<(String, InjectionOutcome)> {
        interaction.execution_success = Some(false);
        interaction.execution_error = Some(message.clone());
        interaction.error_summary = Some(message.clone());
        self.notifier.notify(Notice::EditError(message.clone()));
        stages.timings.inject = StageTiming::skipped(SkipReason::DependencyUnavailable);
        Step::Continue((
            String::new(),
            InjectionOutcome::Failed {
                error: ProtoError::new(ErrorCode::InjectionFailed, message),
            },
        ))
    }

    /// The irreversible stage.
    ///
    /// The commit point is taken *before* any text reaches the injector, so a
    /// cancelled session provably never types anything. Once the guard exists,
    /// a concurrent `cancel` is answered `TooLate` and this runs to completion
    /// rather than leaving half a sentence pasted somewhere.
    async fn inject(
        &self,
        token: &CancelToken,
        stages: &mut Stages,
        opts: &ResolvedOptions,
        text: &str,
    ) -> Step<InjectionOutcome> {
        if !opts.inject {
            // The caller takes delivery instead. A success, not a skip and not
            // a failure — the normal outcome for a remote transcription.
            stages.timings.inject = StageTiming::skipped(SkipReason::NotPermitted);
            return Step::Continue(InjectionOutcome::Delivered);
        }
        if context_policy(&opts.profile) == Some(dictate_inject::InjectionPolicy::Off) {
            stages.timings.inject = StageTiming::skipped(SkipReason::Disabled);
            return Step::Continue(InjectionOutcome::Skipped {
                reason: SkipReason::Disabled,
            });
        }
        // Probing the backend can block — a Wayland portal check is IPC — so
        // it goes to the blocking pool rather than stalling an async worker.
        // It also runs *before* the commit point, which is what keeps a cancel
        // arriving during the probe able to win.
        let available = {
            let injector = self.injector.clone();
            tokio::task::spawn_blocking(move || injector.is_available())
                .await
                .unwrap_or(false)
        };
        if !available {
            stages.timings.inject = StageTiming::skipped(SkipReason::NotSupported);
            return Step::Continue(InjectionOutcome::Unavailable {
                backend: "none".into(),
                reason: "no injection backend is available on this host".into(),
            });
        }
        if text.trim().is_empty() {
            stages.timings.inject = StageTiming::skipped(SkipReason::NoSpeechDetected);
            return Step::Continue(InjectionOutcome::Skipped {
                reason: SkipReason::NoSpeechDetected,
            });
        }

        let Some(guard) = token.enter_commit() else {
            // Cancelled before anything was typed — the whole point of the
            // commit point.
            stages.timings.inject = StageTiming::skipped(SkipReason::Cancelled);
            return Step::Cancelled;
        };

        let clock = StageClock::start();
        // X11 performs its clipboard/key work on the blocking pool inside its
        // adapter. Portal backends instead await an authorization response;
        // keeping the port async preserves both contracts.
        let outcome = self
            .injector
            .inject_bound(
                text,
                context_policy(&opts.profile),
                opts.stop_destination
                    .as_ref()
                    .and_then(|binding| *binding.lock().expect("stop destination poisoned")),
            )
            .await;
        drop(guard);

        stages.timings.inject = match &outcome {
            InjectionOutcome::Failed { error } => clock.failed(error.message.clone()),
            _ => clock.ran(),
        };
        Step::Continue(outcome)
    }

    /// The single teardown path.
    ///
    /// Every exit from [`run`](Self::run) comes through here, which is what
    /// guarantees the two properties cancellation has to have: the capture
    /// stream is always released, and the user's media is always resumed. It
    /// also stamps the timings into the transcript, publishes the terminal
    /// events, and commits the interaction log.
    async fn finish(
        &self,
        handle: &SessionHandle,
        stages: Stages,
        interaction: Option<Interaction>,
        outcome: Outcome,
    ) -> PipelineOutcome {
        // Idempotent, and unconditional on purpose: reaching a terminal state
        // with a live capture stream is the "dangling stream" bug, and the
        // cheapest way to make it impossible is to never rely on having
        // stopped it on the happy path. An upload never touched the device.
        if !handle.is_upload() {
            self.audio.cancel();
        }

        match &outcome.state {
            State::Cancelled => self.earcon(handle, EarconCue::Cancel, AudioActivity::EarconCancel),
            State::Error => self.earcon(handle, EarconCue::Error, AudioActivity::EarconError),
            _ => {}
        }

        let mut timings = stages.finish(outcome.audio_ms);
        // An upload's audio was decoded before the session existed; that cost
        // is in the `capture` stage, so it belongs in the total as well.
        if let (Some(audio), Some(total)) = (handle.supplied_audio(), timings.total_ms.as_mut()) {
            *total += audio.prepare_ms;
        }
        let mut transcript = outcome.transcript;
        if let Some(t) = transcript.as_mut() {
            t.timings = timings.clone();
        }

        match &outcome.state {
            State::Cancelled => self.notifier.notify(Notice::Cancelled),
            State::Error => {}
            _ => self.notifier.notify(Notice::Clear),
        }

        if !handle.is_upload() {
            let media = self.media.clone();
            let resumed = tokio::task::spawn_blocking(move || media.resume_if_needed())
                .await
                .unwrap_or(false);
            if resumed {
                handle.publish(Event::AudioActivity {
                    session_id: handle.id().clone(),
                    activity: AudioActivity::MediaResumed,
                    at_ms: None,
                });
            }
        }

        // Publish the payload events *before* the terminal state change, so a
        // client that stops listening the moment it sees `done` has already
        // received the transcript it was waiting for.
        if let Some(t) = &transcript {
            handle.publish(Event::Final {
                session_id: handle.id().clone(),
                transcript: Box::new(t.clone()),
            });
        }
        if let Some(err) = &outcome.error {
            handle.publish(Event::Error {
                session_id: Some(handle.id().clone()),
                error: err.clone(),
            });
        }

        // A terminal transition is forced rather than checked: the session is
        // over, and refusing to record that because the token is already
        // cancelled would strand the state machine mid-pipeline.
        if let Err(e) = handle.advance(outcome.state.clone()) {
            error!("could not record terminal state: {e}");
        }

        if let Some(mut interaction) = interaction {
            interaction.capture_duration_ms = timings.capture.elapsed_ms();
            interaction.vad_duration_ms = timings.vad.elapsed_ms();
            interaction.stt_duration_ms = timings.stt.elapsed_ms();
            interaction.fmt_rules_duration_ms = timings.fmt_rules.elapsed_ms();
            interaction.fmt_llm_duration_ms = timings.fmt_llm.elapsed_ms();
            interaction.inject_duration_ms = timings.inject.elapsed_ms();
            interaction.word_count = transcript.as_ref().and_then(|t| t.word_count).or_else(|| {
                interaction
                    .corrected_transcription
                    .as_ref()
                    .map(|text| text.split_whitespace().count() as u32)
            });
            if let Some(err) = &outcome.error {
                if interaction.error_summary.is_none() {
                    interaction.error_summary = Some(err.message.clone());
                }
            }
            if outcome.state == State::Cancelled {
                interaction
                    .error_summary
                    .get_or_insert_with(|| "cancelled".into());
            }
            match self.history.lock() {
                Ok(store) if !interaction.no_store => store.commit(&interaction),
                Ok(_) => {}
                Err(e) => error!("history mutex poisoned, dropping interaction: {e}"),
            }
        }

        info!(
            session = %handle.id().as_str(),
            state = %outcome.state.as_str(),
            total_ms = timings.total_ms.unwrap_or_default(),
            "session finished"
        );

        PipelineOutcome {
            state: outcome.state,
            transcript,
            error: outcome.error,
        }
    }

    /// Queue a cue without giving audio-output failure any authority over the
    /// session. The paired event makes successful configured feedback visible
    /// to `dictate tail` and future HUDs.
    fn earcon(&self, handle: &SessionHandle, cue: EarconCue, activity: AudioActivity) {
        // Chimes mark a live recording; an upload has none to mark.
        if !handle.is_upload() && self.earcons.play(cue) {
            handle.publish(Event::AudioActivity {
                session_id: handle.id().clone(),
                activity,
                at_ms: None,
            });
        }
    }
}

/// Internal builder for the terminal outcome, so each early return reads as
/// one line at the call site.
struct Outcome {
    state: State,
    transcript: Option<Transcript>,
    error: Option<ProtoError>,
    audio_ms: Option<f64>,
}

impl Outcome {
    fn cancelled() -> Self {
        Self {
            state: State::Cancelled,
            transcript: None,
            error: None,
            audio_ms: None,
        }
    }

    fn cancelled_after(audio_ms: f64) -> Self {
        Self {
            audio_ms: Some(audio_ms),
            ..Self::cancelled()
        }
    }

    fn done(transcript: Transcript) -> Self {
        Self {
            state: State::Done,
            transcript: Some(transcript),
            error: None,
            audio_ms: None,
        }
    }

    fn error(error: ProtoError, audio_ms: f64) -> Self {
        Self {
            state: State::Error,
            transcript: None,
            error: Some(error),
            audio_ms: Some(audio_ms),
        }
    }

    fn with_audio_ms(mut self, ms: f64) -> Self {
        self.audio_ms = Some(ms);
        self
    }
}

/// A transcript for a session that produced no text.
///
/// The empty string is deliberate and distinct from an absent one: it means
/// "the user said nothing", where absent means "we are not telling you".
fn empty_transcript(reason: SkipReason) -> Transcript {
    Transcript {
        text: FinalText(String::new()),
        raw_text: Some(String::new()),
        route: Route::Type,
        timings: StageTimings::default(),
        injection: InjectionOutcome::Skipped { reason },
        word_count: Some(0),
        model: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_enum_maps_onto_the_wire_enum() {
        assert_eq!(route_to_proto(RouteType::Type), Route::Type);
        assert_eq!(route_to_proto(RouteType::Local), Route::Local);
        assert_eq!(route_to_proto(RouteType::Timer), Route::Timer);
        assert_eq!(route_to_proto(RouteType::Edit), Route::Edit);
        assert_eq!(route_to_proto(RouteType::Command), Route::Command);
    }

    #[test]
    fn stages_start_unreported_until_the_pipeline_runs_them() {
        let s = Stages::new();
        assert_eq!(
            s.timings.vad,
            StageTiming::NotReported,
            "VAD exists, but must not report a fabricated zero before it runs"
        );
        assert_eq!(s.timings.fmt_rules, StageTiming::NotReported);
        assert_eq!(s.timings.stt, StageTiming::NotReported);
        assert_eq!(s.timings.capture, StageTiming::NotReported);
    }

    #[test]
    fn total_ms_is_measured_not_summed() {
        let mut s = Stages::new();
        s.timings.capture = StageTiming::ran(10.0);
        s.timings.stt = StageTiming::ran(20.0);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let t = s.finish(Some(1234.0));
        let total = t.total_ms.expect("total must be reported");
        assert!(
            total >= 5.0,
            "total must be wall time, not the 30ms stage sum"
        );
        assert_eq!(t.audio_ms, Some(1234.0));
    }

    #[test]
    fn a_failed_stage_still_reports_its_cost() {
        let clock = StageClock::start();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let timing = clock.failed("ollama refused the connection");
        match timing {
            StageTiming::Failed { ms, error } => {
                assert!(ms >= 2.0, "time burned before failing must not vanish");
                assert_eq!(error.as_deref(), Some("ollama refused the connection"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_transcript_says_nothing_rather_than_hiding_something() {
        let t = empty_transcript(SkipReason::NoSpeechDetected);
        assert_eq!(t.text.as_str(), "");
        assert_eq!(
            t.raw_text,
            Some(String::new()),
            "empty means the user said nothing; absent would mean privacy mode"
        );
        assert_eq!(t.word_count, Some(0));
    }

    /// The router runs on the rules output (S20), so every trigger must
    /// survive casing, numbers and terminal punctuation.
    fn route_rules_output(raw: &str) -> router::RouteResult {
        let rules = TextChain::default().format(raw, &FormatContext::default());
        router::route(&rules)
    }

    #[test]
    fn a_timer_utterance_routes_and_parses_after_the_rules() {
        let r = route_rules_output("um timer ten minutes");
        assert_eq!(r.route, RouteType::Timer);
        assert_eq!(r.text, "10 minutes");
        assert_eq!(crate::timer::parse_duration(&r.text).0, Some(600));

        let r = route_rules_output("timer twenty five minutes for the tea");
        assert_eq!(r.route, RouteType::Timer);
        let (secs, label) = crate::timer::parse_duration(&r.text);
        assert_eq!(secs, Some(25 * 60), "`twenty five` used to parse as 5");
        assert_eq!(label, "for the tea.");

        let r = route_rules_output("timer one and a half hours");
        assert_eq!(r.route, RouteType::Timer);
        assert_eq!(crate::timer::parse_duration(&r.text).0, Some(5400));
    }

    #[test]
    fn edit_triggers_keep_their_colon_and_local_triggers_still_route() {
        let r = route_rules_output("edit: make this more formal");
        assert_eq!(r.route, RouteType::Edit);
        assert_eq!(r.text, "make this more formal.");

        let r = route_rules_output("um, fix: the typo");
        assert_eq!(r.route, RouteType::Edit);

        let r = route_rules_output("easy what is the capital of France");
        assert_eq!(r.route, RouteType::Local);
        assert_eq!(r.text, "what is the capital of France.");

        let r = route_rules_output("hard, explain monads");
        assert_eq!(r.route, RouteType::Local);

        let r = route_rules_output("research code base for the retry logic");
        assert_eq!(r.route, RouteType::Type);
        assert_eq!(r.text, "/research_codebase for the retry logic.");
    }

    #[test]
    fn default_options_permit_every_known_route() {
        let o = ResolvedOptions::default();
        for route in Route::known() {
            assert!(o.allowed_routes.contains(route));
        }
        assert!(o.inject);
        assert!(!o.privacy);
    }
}
