use anyhow::Result;
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::{error, info, warn};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

#[derive(Debug, Clone)]
pub struct TranscriptionResult {
    pub text: String,
    pub language: String,
    pub duration_s: f64,
}

pub struct Transcriber {
    context: Arc<OnceCell<SendWhisperCtx>>,
    model_path: String,
    use_gpu: bool,
    no_speech_threshold: f32,
}

/// Thread-safe wrapper around WhisperContext.
/// WhisperContext is !Send because it holds FFI pointers, but whisper.cpp
/// is thread-safe for separate state objects created from the same context.
/// We ensure single-threaded access via the signal-serialized pipeline.
struct SendWhisperCtx(WhisperContext);
unsafe impl Send for SendWhisperCtx {}
unsafe impl Sync for SendWhisperCtx {}

impl std::ops::Deref for SendWhisperCtx {
    type Target = WhisperContext;
    fn deref(&self) -> &WhisperContext {
        &self.0
    }
}

impl Transcriber {
    pub fn new(config: &crate::config::WhisperConfig) -> Self {
        // Expand tilde in model path
        let model_path = crate::config::expand_tilde(&config.model_path)
            .to_string_lossy()
            .into_owned();

        Self {
            context: Arc::new(OnceCell::new()),
            model_path,
            use_gpu: config.device == "cuda",
            no_speech_threshold: config.no_speech_threshold,
        }
    }

    /// Start loading the model in the background.
    /// Call this during DictateAgent::new() — first transcribe() will await completion.
    pub fn load_model_async(&self) {
        let ctx = self.context.clone();
        let path = self.model_path.clone();
        let use_gpu = self.use_gpu;

        tokio::spawn(async move {
            let start = std::time::Instant::now();
            info!("Loading Whisper model from {}...", path);

            let result = tokio::task::spawn_blocking(move || {
                let mut params = WhisperContextParameters::default();
                params.use_gpu(use_gpu);
                WhisperContext::new_with_params(&path, params)
            })
            .await;

            match result {
                Ok(Ok(context)) => {
                    let elapsed = start.elapsed();
                    info!("Whisper model loaded in {:.1}s", elapsed.as_secs_f64());
                    let _ = ctx.set(SendWhisperCtx(context));
                }
                Ok(Err(e)) => error!("Failed to load Whisper model: {}", e),
                Err(e) => error!("Model loading task panicked: {}", e),
            }
        });
    }

    /// Check if model is loaded (for status reporting)
    #[allow(dead_code)]
    pub fn is_model_loaded(&self) -> bool {
        self.context.initialized()
    }

    /// Transcribe audio samples (f32, 16kHz mono).
    /// Blocks until model is loaded if still loading.
    pub async fn transcribe(&self, samples: &[f32]) -> Result<Option<TranscriptionResult>> {
        // Wait for model to be available
        let ctx = self
            .context
            .get_or_init(|| async {
                // Fallback: load synchronously if load_model_async wasn't called
                warn!("Model not pre-loaded, loading synchronously...");
                let path = self.model_path.clone();
                let use_gpu = self.use_gpu;
                tokio::task::spawn_blocking(move || {
                    let mut params = WhisperContextParameters::default();
                    params.use_gpu(use_gpu);
                    SendWhisperCtx(
                        WhisperContext::new_with_params(&path, params)
                            .expect("Failed to load Whisper model"),
                    )
                })
                .await
                .expect("Model loading task panicked")
            })
            .await;

        let audio_duration = samples.len() as f64 / 16000.0;
        let samples = samples.to_vec(); // Clone for spawn_blocking move
        let no_speech_thresh = self.no_speech_threshold;

        let start = std::time::Instant::now();

        // whisper-rs is synchronous FFI — run in blocking thread.
        // SendWhisperCtx wraps WhisperContext with Send+Sync since whisper.cpp
        // state objects are safe to use from different threads.
        // We clone the Arc to share the context into the blocking task.
        let ctx_ref = self.context.clone();

        let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let ctx = ctx_ref.get().expect("Model must be loaded by this point");
            let mut state = ctx.create_state().map_err(|e| anyhow::anyhow!("{}", e))?;

            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_language(Some("en"));
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_no_speech_thold(no_speech_thresh);

            state
                .full(params, &samples)
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            let n_segments = state.full_n_segments();
            if n_segments == 0 {
                return Ok(None);
            }

            let mut text = String::new();
            for i in 0..n_segments {
                if let Some(segment) = state.get_segment(i) {
                    if let Ok(segment_text) = segment.to_str_lossy() {
                        text.push_str(&segment_text);
                    }
                }
            }

            let text = text.trim().to_string();
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(text))
            }
        })
        .await??;

        let transcription_time = start.elapsed().as_secs_f64();
        info!(
            "Transcription: {:.3}s for {:.1}s audio",
            transcription_time, audio_duration
        );

        match result {
            Some(text) => {
                let corrected = apply_corrections(&text);
                info!("Transcribed: \"{}\"", corrected);
                Ok(Some(TranscriptionResult {
                    text: corrected,
                    language: "en".into(),
                    duration_s: audio_duration,
                }))
            }
            None => {
                info!("No speech detected");
                Ok(None)
            }
        }
    }
}

/// Hardcoded Whisper mis-transcription corrections.
/// Port of transcribe.py:186-219 — all 23 correction pairs.
/// Applied sequentially with plain string replace (no regex).
fn apply_corrections(text: &str) -> String {
    let corrections = [
        // "Claude" mishear corrections — dot-prefix variants
        (".clod", ".claude"),
        (".cloud", ".claude"),
        (".clawed", ".claude"),
        // "Claude" mishear corrections — space-prefix variants
        (" clod", " claude"),
        (" cloud", " claude"),
        (" clawed", " claude"),
        // "Claude" mishear corrections — capitalized
        ("Clod", "Claude"),
        ("Cloud", "Claude"),
        ("Clawed", "Claude"),
        // Slash command corrections — lowercase
        ("research code base", "/research_codebase"),
        ("research codebase", "/research_codebase"),
        ("create plan", "/create_plan"),
        ("implement plan", "/implement_plan"),
        ("validate plan", "/validate_plan"),
        ("create handoff", "/create_handoff"),
        ("create hand off", "/create_handoff"),
        // Slash command corrections — capitalized
        ("Research code base", "/research_codebase"),
        ("Research codebase", "/research_codebase"),
        ("Create plan", "/create_plan"),
        ("Implement plan", "/implement_plan"),
        ("Validate plan", "/validate_plan"),
        ("Create handoff", "/create_handoff"),
        ("Create hand off", "/create_handoff"),
    ];

    let mut result = text.to_string();
    for (from, to) in corrections {
        result = result.replace(from, to);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_corrections_claude_names() {
        assert_eq!(apply_corrections("Hello Clod"), "Hello Claude");
        assert_eq!(apply_corrections("Hello Cloud"), "Hello Claude");
        assert_eq!(apply_corrections("Hello Clawed"), "Hello Claude");
    }

    #[test]
    fn test_corrections_claude_dot_prefix() {
        assert_eq!(apply_corrections("hey.clod"), "hey.claude");
        assert_eq!(apply_corrections("hey.cloud"), "hey.claude");
    }

    #[test]
    fn test_corrections_claude_space_prefix() {
        assert_eq!(apply_corrections("hey clod"), "hey claude");
        assert_eq!(apply_corrections("hey cloud"), "hey claude");
    }

    #[test]
    fn test_corrections_slash_commands() {
        assert_eq!(apply_corrections("research codebase"), "/research_codebase");
        assert_eq!(
            apply_corrections("research code base"),
            "/research_codebase"
        );
        assert_eq!(apply_corrections("create plan"), "/create_plan");
        assert_eq!(apply_corrections("implement plan"), "/implement_plan");
        assert_eq!(apply_corrections("validate plan"), "/validate_plan");
        assert_eq!(apply_corrections("create handoff"), "/create_handoff");
        assert_eq!(apply_corrections("create hand off"), "/create_handoff");
    }

    #[test]
    fn test_corrections_slash_commands_capitalized() {
        assert_eq!(
            apply_corrections("Research codebase"),
            "/research_codebase"
        );
        assert_eq!(apply_corrections("Create plan"), "/create_plan");
        assert_eq!(apply_corrections("Implement plan"), "/implement_plan");
        assert_eq!(apply_corrections("Validate plan"), "/validate_plan");
        assert_eq!(apply_corrections("Create handoff"), "/create_handoff");
        assert_eq!(apply_corrections("Create hand off"), "/create_handoff");
    }

    #[test]
    fn test_corrections_no_match_passthrough() {
        assert_eq!(apply_corrections("hello world"), "hello world");
        assert_eq!(apply_corrections(""), "");
    }

    #[test]
    fn test_corrections_multiple_in_one_string() {
        assert_eq!(
            apply_corrections("Hey Clod please create plan"),
            "Hey Claude please /create_plan"
        );
    }
}
