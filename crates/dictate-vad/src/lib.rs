//! CPU-capable Silero VAD gating for Dictate Agent.
//!
//! Silero v5 requires 16 kHz mono audio in 512-sample windows. This crate
//! owns that constraint, returns only the useful speech span to STT, and
//! provides a stateful tracker for hands-free end-of-speech detection.

use anyhow::{bail, Result};
use serde::Deserialize;
use voice_activity_detector::VoiceActivityDetector;

/// The only sample rate supported by Dictate Agent's capture path and Silero
/// v5's wide-band model.
pub const SAMPLE_RATE_HZ: usize = 16_000;
/// Silero v5's mandatory window at [`SAMPLE_RATE_HZ`].
pub const WINDOW_SAMPLES: usize = 512;

/// Configuration for speech gating, trim padding, and hands-free stopping.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VadConfig {
    /// Enables VAD. Disabled is intentionally a pass-through so accessibility
    /// and unusual audio sources still have an explicit escape hatch.
    pub enabled: bool,
    /// Silero speech probability at or above which a frame is speech.
    pub speech_threshold: f32,
    /// Require this much cumulative speech before passing audio to STT.
    pub min_speech_ms: u32,
    /// Keep this much audio around either side of detected speech.
    pub speech_pad_ms: u32,
    /// One-shot sessions stop after this much silence following speech.
    pub trailing_silence_ms: u32,
    /// Frequency the core polls capture snapshots for one-shot auto-stop.
    pub poll_interval_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            speech_threshold: 0.5,
            min_speech_ms: 120,
            speech_pad_ms: 100,
            trailing_silence_ms: 900,
            poll_interval_ms: 32,
        }
    }
}

impl VadConfig {
    /// Reject configuration that would make a gate decision ambiguous.
    pub fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.speech_threshold) {
            bail!("vad.speech_threshold must be within 0.0..=1.0");
        }
        if self.min_speech_ms == 0 {
            bail!("vad.min_speech_ms must be greater than zero");
        }
        if self.trailing_silence_ms == 0 {
            bail!("vad.trailing_silence_ms must be greater than zero");
        }
        if self.poll_interval_ms == 0 {
            bail!("vad.poll_interval_ms must be greater than zero");
        }
        Ok(())
    }
}

/// Result of gating a completed capture.
#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    /// No qualifying speech was found. The caller must not invoke STT.
    NoSpeech,
    /// Speech was found; only this padded span should be passed to STT.
    Speech {
        /// Trimmed, 16 kHz mono samples.
        samples: Vec<f32>,
        /// Input removed before the retained span.
        leading_trimmed_ms: f64,
        /// Input removed after the retained span.
        trailing_trimmed_ms: f64,
    },
}

/// Narrow port consumed by the core pipeline. Tests can use a deterministic
/// implementation without loading ONNX Runtime.
pub trait VoiceActivityGate: Send + Sync + 'static {
    /// Gate and trim a completed capture.
    fn gate(&self, samples: &[f32]) -> Result<GateDecision>;
    /// Build per-session state for incremental end-of-speech tracking.
    fn trailing_silence_tracker(&self) -> Result<Box<dyn TrailingSilenceTracker>>;
    /// Whether this implementation is deliberately bypassing VAD.
    fn enabled(&self) -> bool;
    /// Poll interval requested for the live auto-stop loop.
    fn poll_interval_ms(&self) -> u32;
}

/// Incremental detector used while a one-shot recording remains open.
pub trait TrailingSilenceTracker: Send {
    /// Observe a whole capture snapshot. Returns true only after speech has
    /// been observed and the configured trailing silence elapsed.
    fn observe_snapshot(&mut self, samples: &[f32]) -> Result<bool>;
}

/// The Silero v5 implementation, entirely CPU-capable.
#[derive(Debug, Clone)]
pub struct SileroVad {
    config: VadConfig,
}

impl SileroVad {
    /// Construct a validated gate. Model initialization is deferred to a
    /// session, avoiding daemon-start failures on an unused VAD path.
    pub fn new(config: VadConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }

    fn detector() -> Result<VoiceActivityDetector> {
        Ok(VoiceActivityDetector::builder()
            .sample_rate(SAMPLE_RATE_HZ as i64)
            .chunk_size(WINDOW_SAMPLES)
            .build()?)
    }

    fn frames(&self, samples: &[f32]) -> Result<Vec<bool>> {
        let mut detector = Self::detector()?;
        Ok(samples
            .chunks(WINDOW_SAMPLES)
            .map(|chunk| detector.predict(chunk.iter().copied()) >= self.config.speech_threshold)
            .collect())
    }
}

impl VoiceActivityGate for SileroVad {
    fn gate(&self, samples: &[f32]) -> Result<GateDecision> {
        if !self.config.enabled {
            return Ok(GateDecision::Speech {
                samples: samples.to_vec(),
                leading_trimmed_ms: 0.0,
                trailing_trimmed_ms: 0.0,
            });
        }
        if samples.is_empty() {
            return Ok(GateDecision::NoSpeech);
        }

        let frames = self.frames(samples)?;
        let speech_frames: Vec<_> = frames
            .iter()
            .enumerate()
            .filter_map(|(index, speech)| speech.then_some(index))
            .collect();
        let speech_samples = speech_frames.len() * WINDOW_SAMPLES;
        if speech_samples * 1000 < self.config.min_speech_ms as usize * SAMPLE_RATE_HZ {
            return Ok(GateDecision::NoSpeech);
        }

        let first = *speech_frames.first().expect("non-empty after length check");
        let last = *speech_frames.last().expect("non-empty after length check");
        let pad = self.config.speech_pad_ms as usize * SAMPLE_RATE_HZ / 1000;
        let start = first.saturating_mul(WINDOW_SAMPLES).saturating_sub(pad);
        let end = ((last + 1) * WINDOW_SAMPLES + pad).min(samples.len());
        Ok(GateDecision::Speech {
            samples: samples[start..end].to_vec(),
            leading_trimmed_ms: samples_to_ms(start),
            trailing_trimmed_ms: samples_to_ms(samples.len() - end),
        })
    }

    fn trailing_silence_tracker(&self) -> Result<Box<dyn TrailingSilenceTracker>> {
        Ok(Box::new(SileroTrailingSilence {
            detector: Self::detector()?,
            config: self.config.clone(),
            processed_frames: 0,
            saw_speech: false,
            silence_frames: 0,
        }))
    }

    fn enabled(&self) -> bool {
        self.config.enabled
    }

    fn poll_interval_ms(&self) -> u32 {
        self.config.poll_interval_ms
    }
}

struct SileroTrailingSilence {
    detector: VoiceActivityDetector,
    config: VadConfig,
    processed_frames: usize,
    saw_speech: bool,
    silence_frames: usize,
}

impl TrailingSilenceTracker for SileroTrailingSilence {
    fn observe_snapshot(&mut self, samples: &[f32]) -> Result<bool> {
        if !self.config.enabled {
            return Ok(false);
        }
        let available_frames = samples.len() / WINDOW_SAMPLES;
        while self.processed_frames < available_frames {
            let start = self.processed_frames * WINDOW_SAMPLES;
            let speech = self
                .detector
                .predict(samples[start..start + WINDOW_SAMPLES].iter().copied())
                >= self.config.speech_threshold;
            self.processed_frames += 1;
            if speech {
                self.saw_speech = true;
                self.silence_frames = 0;
            } else if self.saw_speech {
                self.silence_frames += 1;
                if self.silence_frames * WINDOW_SAMPLES * 1000
                    >= self.config.trailing_silence_ms as usize * SAMPLE_RATE_HZ
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn samples_to_ms(samples: usize) -> f64 {
    samples as f64 / SAMPLE_RATE_HZ as f64 * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validation_rejects_invalid_thresholds() {
        assert!(SileroVad::new(VadConfig {
            speech_threshold: 1.01,
            ..VadConfig::default()
        })
        .is_err());
        assert!(SileroVad::new(VadConfig {
            trailing_silence_ms: 0,
            ..VadConfig::default()
        })
        .is_err());
    }

    #[test]
    fn empty_input_is_no_speech_without_initializing_onnx() {
        let vad = SileroVad::new(VadConfig::default()).unwrap();
        assert_eq!(vad.gate(&[]).unwrap(), GateDecision::NoSpeech);
    }

    #[test]
    fn disabled_vad_is_explicit_pass_through() {
        let vad = SileroVad::new(VadConfig {
            enabled: false,
            ..VadConfig::default()
        })
        .unwrap();
        let input = vec![0.0; 17];
        assert_eq!(
            vad.gate(&input).unwrap(),
            GateDecision::Speech {
                samples: input,
                leading_trimmed_ms: 0.0,
                trailing_trimmed_ms: 0.0
            }
        );
    }
}
