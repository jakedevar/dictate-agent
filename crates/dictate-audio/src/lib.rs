//! Local microphone capture primitives.
//!
//! The stream stays armed while the daemon is idle.  Its bounded ring buffer
//! therefore contains the few hundred milliseconds before a hotkey reaches
//! the engine, avoiding first-syllable clipping without making the engine a
//! continuously-recording session.

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, StreamConfig};
use rodio::{source::SineWave, OutputStream, Sink, Source};
use serde::Deserialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info, warn};

/// The fixed format sent to Whisper.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// A bounded FIFO used by the armed microphone stream.
#[derive(Debug, Clone)]
pub struct SampleRing {
    capacity: usize,
    samples: VecDeque<f32>,
}

impl SampleRing {
    /// Make a ring that holds approximately `duration_ms` at `sample_rate`.
    #[must_use]
    pub fn for_duration(sample_rate: u32, duration_ms: u32) -> Self {
        let capacity = ((u64::from(sample_rate) * u64::from(duration_ms)) / 1_000) as usize;
        Self::with_capacity(capacity)
    }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            samples: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, sample: f32) {
        if self.capacity == 0 {
            return;
        }
        if self.samples.len() == self.capacity {
            let _ = self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn extend(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.push(sample);
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<f32> {
        self.samples.iter().copied().collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Software gain controls.  The limiter is deliberately simple: it is a
/// fail-safe for quiet speech, not S11's VAD/noise-suppression work.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GainConfig {
    /// Apply RMS-normalizing gain after capture.
    pub enabled: bool,
    /// Desired RMS of voiced, quiet speech.
    pub target_rms: f32,
    /// Never attenuate ordinary speech below this multiplier.
    pub min_gain: f32,
    /// Bound boosted room noise and accidental clicks.
    pub max_gain: f32,
    /// Signals below this RMS are treated as silence, never amplified.
    pub silence_rms: f32,
}

impl Default for GainConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target_rms: 0.12,
            min_gain: 1.0,
            max_gain: 8.0,
            silence_rms: 0.002,
        }
    }
}

/// Capture behavior kept independent of the daemon so fixture tests do not
/// need a microphone or PipeWire.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Exact CPAL input-device name. An empty value uses the system default.
    pub input_device: String,
    /// Keep this much audio immediately before a hotkey.
    pub pre_roll_ms: u32,
    /// Preserve the legacy end-of-utterance grace period.
    pub trailing_capture_ms: u32,
    /// Reopen the configured/default device on the next start after a stream error.
    pub hotplug_recovery: bool,
    pub gain: GainConfig,
    /// Warn only after this much captured audio is effectively silent.
    pub mic_mute_warning: bool,
    pub mic_mute_min_ms: u32,
    pub mic_mute_rms: f32,
    pub earcons: EarconConfig,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            input_device: String::new(),
            pre_roll_ms: 300,
            trailing_capture_ms: 500,
            hotplug_recovery: true,
            gain: GainConfig::default(),
            mic_mute_warning: true,
            mic_mute_min_ms: 900,
            mic_mute_rms: 0.0015,
            // Existing installs were silent. Opt in to chimes explicitly.
            earcons: EarconConfig::default(),
        }
    }
}

/// Optional Wispr-style chimes.  They are non-blocking and never affect a
/// recording or a pipeline result when the audio output is unavailable.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EarconConfig {
    pub enabled: bool,
    pub volume: f32,
}

impl Default for EarconConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            volume: 0.18,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarconCue {
    Start,
    Stop,
    Cancel,
    Error,
}

/// Rodio-backed earcon player.  The caller only learns whether the cue was
/// queued; actual desktop audio remains intentionally best-effort.
#[derive(Debug, Clone)]
pub struct EarconPlayer {
    config: EarconConfig,
}

impl EarconPlayer {
    #[must_use]
    pub fn new(config: EarconConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn play(&self, cue: EarconCue) -> bool {
        if !self.config.enabled {
            return false;
        }
        let volume = self.config.volume.clamp(0.0, 1.0);
        std::thread::spawn(move || {
            let Ok((_stream, handle)) = OutputStream::try_default() else {
                return;
            };
            let Ok(sink) = Sink::try_new(&handle) else {
                return;
            };
            let (frequency, duration) = match cue {
                EarconCue::Start => (880, 55),
                EarconCue::Stop => (660, 55),
                EarconCue::Cancel => (330, 85),
                EarconCue::Error => (220, 110),
            };
            sink.set_volume(volume);
            sink.append(
                SineWave::new(frequency as f32).take_duration(Duration::from_millis(duration)),
            );
            sink.sleep_until_end();
        });
        true
    }
}

/// Measured from raw samples before AGC changes their amplitude.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CaptureDiagnostics {
    pub input_rms: f32,
    pub applied_gain: f32,
    pub mic_mute_suspected: bool,
    pub recovered_device: bool,
}

#[derive(Debug)]
struct CaptureState {
    ring: SampleRing,
    samples: Vec<f32>,
    recording: bool,
}

/// Pick a configured device name, falling back to the default only when no
/// name was requested.  Kept pure so device-selection behavior is testable on
/// a CI host with no ALSA/PipeWire devices.
pub fn choose_device_name<'a>(
    available: &'a [String],
    requested: &str,
    default: Option<&'a str>,
) -> Result<&'a str> {
    if !requested.trim().is_empty() {
        return available
            .iter()
            .find(|name| name.as_str() == requested)
            .map(String::as_str)
            .ok_or_else(|| anyhow!("configured input device '{requested}' was not found"));
    }
    default.ok_or_else(|| anyhow!("no input device found"))
}

/// Returns the raw RMS. It is public for fixtures and the mute warning.
#[must_use]
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Apply bounded automatic gain and return the gain actually applied.
pub fn apply_gain(samples: &mut [f32], config: &GainConfig) -> f32 {
    if !config.enabled {
        return 1.0;
    }
    let input_rms = rms(samples);
    if input_rms <= config.silence_rms || !input_rms.is_finite() {
        return 1.0;
    }
    let min = config.min_gain.max(0.0);
    let max = config.max_gain.max(min);
    let gain = (config.target_rms / input_rms).clamp(min, max);
    for sample in samples {
        *sample = (*sample * gain).clamp(-1.0, 1.0);
    }
    gain
}

#[must_use]
fn recovery_required(has_stream: bool, stream_failed: bool) -> bool {
    !has_stream || stream_failed
}

#[must_use]
fn mute_warning(duration_ms: f32, input_rms: f32, config: &AudioConfig) -> bool {
    config.mic_mute_warning
        && duration_ms >= config.mic_mute_min_ms as f32
        && input_rms <= config.mic_mute_rms
}

/// The stream's error callback cannot safely reopen a CPAL stream itself.
/// This tiny state machine records that fact for `start()` to recover safely.
#[derive(Debug, Default)]
struct StreamHealth {
    failed: AtomicBool,
}

pub struct AudioCapture {
    config: AudioConfig,
    stream: Option<cpal::Stream>,
    state: Arc<Mutex<CaptureState>>,
    health: Arc<StreamHealth>,
    last_diagnostics: CaptureDiagnostics,
}

impl AudioCapture {
    pub fn new(config: AudioConfig) -> Result<Self> {
        let state = Arc::new(Mutex::new(CaptureState {
            ring: SampleRing::for_duration(SAMPLE_RATE_HZ, config.pre_roll_ms),
            samples: Vec::new(),
            recording: false,
        }));
        let mut capture = Self {
            config,
            stream: None,
            state,
            health: Arc::new(StreamHealth::default()),
            last_diagnostics: CaptureDiagnostics::default(),
        };

        // Being armed is what makes pre-roll real. Failure is not fatal at
        // daemon startup: a USB microphone may be plugged in later, and the
        // next start synchronously reports an actionable device error if it
        // still cannot recover.
        if let Err(error) = capture.arm() {
            warn!(%error, "audio input is not armed yet; start will retry");
        }
        Ok(capture)
    }

    fn device(&self, host: &cpal::Host) -> Result<cpal::Device> {
        let requested = self.config.input_device.trim();
        if requested.is_empty() {
            return host
                .default_input_device()
                .ok_or_else(|| anyhow!("no input device found"));
        }
        host.input_devices()?
            .find(|device| device.name().ok().as_deref() == Some(requested))
            .ok_or_else(|| anyhow!("configured input device '{requested}' was not found"))
    }

    fn input_config(device: &cpal::Device) -> Result<(SampleFormat, StreamConfig)> {
        let configs: Vec<_> = device.supported_input_configs()?.collect();
        for format in [SampleFormat::F32, SampleFormat::I16] {
            // Prefer a real mono stream. If the device exposes only an
            // interleaved multi-channel format, the callback downmixes it.
            for mono_only in [true, false] {
                if let Some(config) = configs.iter().find(|config| {
                    config.sample_format() == format
                        && config.min_sample_rate().0 <= SAMPLE_RATE_HZ
                        && config.max_sample_rate().0 >= SAMPLE_RATE_HZ
                        && (!mono_only || config.channels() == 1)
                }) {
                    return Ok((
                        format,
                        config.with_sample_rate(SampleRate(SAMPLE_RATE_HZ)).config(),
                    ));
                }
            }
        }
        bail!("input device supports neither 16 kHz f32 nor i16 capture")
    }

    /// Open or reopen the input stream. Call only on the capture owner thread.
    fn arm(&mut self) -> Result<()> {
        self.stream.take();
        let host = cpal::default_host();
        let device = self.device(&host)?;
        let name = device.name().unwrap_or_else(|_| "unnamed input".into());
        let (format, stream_config) = Self::input_config(&device)?;
        let channels = usize::from(stream_config.channels);
        let state = self.state.clone();
        let health = self.health.clone();
        health.failed.store(false, Ordering::Release);
        let error_callback = move |stream_error| {
            error!(%stream_error, "audio input stream failed; recovery is pending");
            health.failed.store(true, Ordering::Release);
        };
        let stream = match format {
            SampleFormat::F32 => device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| record_interleaved(&state, data, channels),
                error_callback,
                None,
            )?,
            SampleFormat::I16 => {
                let state = self.state.clone();
                let health = self.health.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _| {
                        let mut converted = Vec::with_capacity(data.len());
                        converted.extend(data.iter().map(|sample| f32::from(*sample) / 32768.0));
                        record_interleaved(&state, &converted, channels);
                    },
                    move |stream_error| {
                        error!(%stream_error, "audio input stream failed; recovery is pending");
                        health.failed.store(true, Ordering::Release);
                    },
                    None,
                )?
            }
            _ => unreachable!("input_config selects only f32 or i16"),
        };
        stream.play()?;
        self.stream = Some(stream);
        info!(device = %name, "audio input armed for pre-roll");
        Ok(())
    }

    pub fn start(&mut self) -> Result<()> {
        if self.is_recording() {
            bail!("Already recording");
        }
        let recovered = recovery_required(
            self.stream.is_some(),
            self.health.failed.load(Ordering::Acquire),
        );
        if recovered {
            if !self.config.hotplug_recovery {
                bail!("audio input is unavailable and hotplug recovery is disabled");
            }
            self.arm().context("recovering audio input device")?;
        }
        let mut state = self.state.lock().expect("audio capture state poisoned");
        state.samples = state.ring.snapshot();
        state.recording = true;
        self.last_diagnostics = CaptureDiagnostics {
            recovered_device: recovered,
            ..CaptureDiagnostics::default()
        };
        info!(pre_roll_samples = state.samples.len(), "recording started");
        Ok(())
    }

    /// Stops recording and returns 16 kHz mono f32 samples, including pre-roll.
    pub async fn stop(&mut self) -> Option<Vec<f32>> {
        if !self.is_recording() {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(u64::from(
            self.config.trailing_capture_ms,
        )))
        .await;
        let mut samples = {
            let mut state = self.state.lock().expect("audio capture state poisoned");
            state.recording = false;
            let samples = std::mem::take(&mut state.samples);
            // A cancelled/completed utterance must not leak back into a rapid
            // next session through the idle pre-roll ring.
            state.ring.clear();
            samples
        };
        let input_rms = rms(&samples);
        let duration_ms = samples.len() as f32 / SAMPLE_RATE_HZ as f32 * 1_000.0;
        let applied_gain = apply_gain(&mut samples, &self.config.gain);
        self.last_diagnostics.input_rms = input_rms;
        self.last_diagnostics.applied_gain = applied_gain;
        self.last_diagnostics.mic_mute_suspected =
            mute_warning(duration_ms, input_rms, &self.config);
        info!(duration_ms, input_rms, applied_gain, "recording stopped");
        (!samples.is_empty()).then_some(samples)
    }

    /// Cancel recording while leaving the armed stream available for the next hotkey.
    pub fn cancel(&mut self) {
        let mut state = self.state.lock().expect("audio capture state poisoned");
        state.recording = false;
        state.samples.clear();
        state.ring.clear();
    }

    /// Copy samples collected so far without interrupting capture.
    pub fn snapshot(&self) -> Vec<f32> {
        self.state
            .lock()
            .map(|state| state.samples.clone())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn is_recording(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.recording)
            .unwrap_or(false)
    }

    #[must_use]
    pub fn diagnostics(&self) -> CaptureDiagnostics {
        self.last_diagnostics
    }

    #[must_use]
    pub fn duration_secs(samples: &[f32]) -> f64 {
        samples.len() as f64 / f64::from(SAMPLE_RATE_HZ)
    }
}

fn record_samples(state: &Arc<Mutex<CaptureState>>, data: &[f32]) {
    let Ok(mut state) = state.lock() else {
        return;
    };
    state.ring.extend(data);
    if state.recording {
        state.samples.extend_from_slice(data);
    }
}

fn record_interleaved(state: &Arc<Mutex<CaptureState>>, data: &[f32], channels: usize) {
    if channels <= 1 {
        record_samples(state, data);
        return;
    }
    let mono: Vec<f32> = data
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    record_samples(state, &mono);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(_name: &str) -> Vec<f32> {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/quiet-speech.csv"
        ))
        .lines()
        .filter(|line| !line.starts_with('#'))
        .flat_map(|line| line.split(','))
        .filter_map(|value| value.trim().parse::<f32>().ok())
        .collect::<Vec<_>>()
    }

    #[test]
    fn ring_retains_the_latest_preroll_only() {
        let mut ring = SampleRing::with_capacity(4);
        ring.extend(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(ring.len(), 4);
        assert_eq!(ring.snapshot(), vec![2.0, 3.0, 4.0, 5.0]);
        ring.clear();
        assert!(ring.is_empty());
    }

    #[test]
    fn multichannel_capture_is_downmixed_to_mono() {
        let state = Arc::new(Mutex::new(CaptureState {
            ring: SampleRing::with_capacity(8),
            samples: Vec::new(),
            recording: true,
        }));
        record_interleaved(&state, &[1.0, -1.0, 0.5, 0.25], 2);
        assert_eq!(state.lock().unwrap().samples, vec![0.0, 0.375]);
    }

    #[test]
    fn preroll_capacity_is_300ms_at_the_pipeline_rate() {
        assert_eq!(
            SampleRing::for_duration(SAMPLE_RATE_HZ, 300).capacity,
            4_800
        );
    }

    #[test]
    fn fixture_quiet_speech_is_boosted_but_limited() {
        let mut samples = fixture("quiet-speech");
        let before = rms(&samples);
        let gain = apply_gain(&mut samples, &GainConfig::default());
        assert!(before > GainConfig::default().silence_rms);
        assert!(gain > 1.0 && gain <= GainConfig::default().max_gain);
        assert!(rms(&samples) > before);
        assert!(samples.iter().all(|sample| sample.abs() <= 1.0));
    }

    #[test]
    fn silence_is_not_amplified_into_noise() {
        let mut samples = vec![0.0005; 32];
        assert_eq!(apply_gain(&mut samples, &GainConfig::default()), 1.0);
        assert_eq!(samples, vec![0.0005; 32]);
    }

    #[test]
    fn configured_device_is_exact_and_default_is_used_only_when_unset() {
        let devices = vec!["USB Mic".into(), "Laptop Mic".into()];
        assert_eq!(
            choose_device_name(&devices, "USB Mic", Some("Laptop Mic")).unwrap(),
            "USB Mic"
        );
        assert_eq!(
            choose_device_name(&devices, "", Some("Laptop Mic")).unwrap(),
            "Laptop Mic"
        );
        assert!(choose_device_name(&devices, "Missing", Some("Laptop Mic")).is_err());
    }

    #[test]
    fn hotplug_failure_requires_a_safe_reopen_on_the_next_start() {
        assert!(recovery_required(false, false));
        assert!(recovery_required(true, true));
        assert!(!recovery_required(true, false));
    }

    #[test]
    fn mute_warning_requires_long_enough_near_silence() {
        let config = AudioConfig::default();
        assert!(mute_warning(900.0, 0.001, &config));
        assert!(!mute_warning(100.0, 0.001, &config));
        assert!(!mute_warning(900.0, 0.01, &config));
    }

    #[test]
    fn test_duration_secs() {
        assert!((AudioCapture::duration_secs(&vec![0.0; 16_000]) - 1.0).abs() < f64::EPSILON);
        assert!((AudioCapture::duration_secs(&[]) - 0.0).abs() < f64::EPSILON);
    }
}
