pub mod config;
pub mod model;
pub mod transcribe;

pub use config::WhisperConfig;
pub use model::{catalog_model, ModelManager, ModelSpec, ModelStatus, CATALOG};
pub use transcribe::{Transcriber, WhisperStt};

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;

/// Object-safe async return used by the provider contract.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Per-call overrides. Empty values inherit the configured defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SttRequest {
    /// `None` asks Whisper to auto-detect. A pinned language avoids detection
    /// ambiguity and can shave a little decode work for known-language users.
    pub language: Option<String>,
    /// Dictionary/vocabulary bias. S22 owns assembling this string.
    pub initial_prompt: Option<String>,
    /// Override the configured no-speech probability threshold (0.0..=1.0).
    pub no_speech_threshold: Option<f32>,
}

/// The recognizer result, including the timing data that makes latency reads
/// meaningful across CUDA and CPU fallback.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcription {
    pub text: String,
    pub language: Option<String>,
    pub timings: SttTimings,
}

/// Timings measured around one provider request, in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SttTimings {
    pub audio_ms: f64,
    pub queue_ms: f64,
    pub model_load_ms: f64,
    pub decode_ms: f64,
    pub total_ms: f64,
}

/// Actual loaded model status. `backend` is selected at load time, never just
/// echoed from configuration, because CUDA and CPU latency are incomparable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelInfo {
    pub name: String,
    pub loaded: bool,
    pub backend: Option<String>,
}

/// Final extension point for local, future remote, or future cloud STT.
pub trait SttProvider: Send + Sync + 'static {
    /// Transcribe 16 kHz mono f32 audio. `Ok(None)` means no speech.
    fn transcribe<'a>(
        &'a self,
        samples: &'a [f32],
        request: SttRequest,
    ) -> BoxFuture<'a, Result<Option<Transcription>>>;

    fn model(&self) -> ModelInfo;
}
