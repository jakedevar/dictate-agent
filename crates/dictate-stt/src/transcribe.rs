use anyhow::{anyhow, Result};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use tracing::{error, info};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use dictate_fmt::text_cleanup::scrub_returned_text;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TranscriptionResult {
    pub text: String,
    pub language: String,
    pub duration_s: f64,
}

pub struct Transcriber {
    worker_tx: mpsc::Sender<WorkerMessage>,
    worker: Option<std::thread::JoinHandle<()>>,
    model_loaded: Arc<AtomicBool>,
    no_speech_threshold: f32,
}

enum WorkerMessage {
    Preload,
    Transcribe {
        samples: Vec<f32>,
        no_speech_threshold: f32,
        reply: tokio::sync::oneshot::Sender<Result<Option<String>>>,
    },
    Shutdown,
}

struct WorkerModel {
    state: WhisperState,
}

impl Transcriber {
    pub fn new(config: &crate::config::WhisperConfig) -> Self {
        // Expand tilde in model path
        let model_path = crate::config::expand_tilde(&config.model_path)
            .to_string_lossy()
            .into_owned();

        let use_gpu = config.device == "cuda";
        let no_speech_threshold = config.no_speech_threshold;
        let (worker_tx, worker_rx) = mpsc::channel();
        let model_loaded = Arc::new(AtomicBool::new(false));
        let worker_model_loaded = Arc::clone(&model_loaded);
        let worker_path = model_path.clone();

        let worker = std::thread::Builder::new()
            .name("dictate-whisper".into())
            .spawn(move || whisper_worker(worker_rx, worker_path, use_gpu, worker_model_loaded))
            .expect("failed to start Whisper worker thread");

        Self {
            worker_tx,
            worker: Some(worker),
            model_loaded,
            no_speech_threshold,
        }
    }

    /// Start loading the model in the background.
    /// Call this during DictateAgent::new() — first transcribe() will queue behind it.
    pub fn load_model_async(&self) {
        if let Err(e) = self.worker_tx.send(WorkerMessage::Preload) {
            error!("Failed to queue Whisper preload: {}", e);
        }
    }

    /// Check if model is loaded (for status reporting)
    #[allow(dead_code)]
    pub fn is_model_loaded(&self) -> bool {
        self.model_loaded.load(Ordering::Acquire)
    }

    /// Transcribe audio samples (f32, 16kHz mono).
    /// Blocks until model is loaded if still loading.
    pub async fn transcribe(&self, samples: &[f32]) -> Result<Option<TranscriptionResult>> {
        let audio_duration = samples.len() as f64 / 16000.0;
        let start = std::time::Instant::now();

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.worker_tx
            .send(WorkerMessage::Transcribe {
                samples: samples.to_vec(),
                no_speech_threshold: self.no_speech_threshold,
                reply: reply_tx,
            })
            .map_err(|_| anyhow!("Whisper worker is not running"))?;

        let result = reply_rx
            .await
            .map_err(|_| anyhow!("Whisper worker stopped before returning a transcription"))??;

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

impl Drop for Transcriber {
    fn drop(&mut self) {
        let _ = self.worker_tx.send(WorkerMessage::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn whisper_worker(
    rx: mpsc::Receiver<WorkerMessage>,
    model_path: String,
    use_gpu: bool,
    model_loaded: Arc<AtomicBool>,
) {
    let mut model: Option<WorkerModel> = None;

    while let Ok(message) = rx.recv() {
        match message {
            WorkerMessage::Preload => {
                let _ = ensure_model_loaded(&mut model, &model_path, use_gpu, &model_loaded);
            }
            WorkerMessage::Transcribe {
                samples,
                no_speech_threshold,
                reply,
            } => {
                let result = ensure_model_loaded(&mut model, &model_path, use_gpu, &model_loaded)
                    .and_then(|model| transcribe_with_model(model, &samples, no_speech_threshold));
                let _ = reply.send(result);
            }
            WorkerMessage::Shutdown => break,
        }
    }
}

fn ensure_model_loaded<'a>(
    model: &'a mut Option<WorkerModel>,
    model_path: &str,
    use_gpu: bool,
    model_loaded: &AtomicBool,
) -> Result<&'a mut WorkerModel> {
    if model.is_none() {
        let start = std::time::Instant::now();
        info!("Loading Whisper model from {}...", model_path);

        match load_worker_model(model_path, use_gpu) {
            Ok(loaded) => {
                info!(
                    "Whisper model loaded in {:.1}s",
                    start.elapsed().as_secs_f64()
                );
                model_loaded.store(true, Ordering::Release);
                *model = Some(loaded);
            }
            Err(e) => {
                model_loaded.store(false, Ordering::Release);
                error!("Failed to load Whisper model: {}", e);
                return Err(e);
            }
        }
    }

    model
        .as_mut()
        .ok_or_else(|| anyhow!("Whisper model is not loaded"))
}

fn load_worker_model(model_path: &str, use_gpu: bool) -> Result<WorkerModel> {
    let mut params = WhisperContextParameters::default();
    params.use_gpu(use_gpu);

    let context = WhisperContext::new_with_params(model_path, params)
        .map_err(|e| anyhow!("failed to load Whisper context: {}", e))?;
    let state = context
        .create_state()
        .map_err(|e| anyhow!("failed to initialize Whisper state: {}", e))?;

    Ok(WorkerModel { state })
}

fn transcribe_with_model(
    model: &mut WorkerModel,
    samples: &[f32],
    no_speech_threshold: f32,
) -> Result<Option<String>> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_no_context(true);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_no_speech_thold(no_speech_threshold);

    model
        .state
        .full(params, samples)
        .map_err(|e| anyhow!("{}", e))?;

    let n_segments = model.state.full_n_segments();
    if n_segments == 0 {
        return Ok(None);
    }

    let mut text = String::new();
    for i in 0..n_segments {
        if let Some(segment) = model.state.get_segment(i) {
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
    scrub_returned_text(&result)
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
        assert_eq!(apply_corrections("Research codebase"), "/research_codebase");
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
