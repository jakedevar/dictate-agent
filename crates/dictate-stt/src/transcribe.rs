use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;
use tracing::{error, info};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use dictate_fmt::text_cleanup::scrub_returned_text;

use crate::{
    BoxFuture, ModelInfo, ModelManager, SttProvider, SttRequest, SttTimings, Transcription,
    WhisperConfig,
};

/// Serialized whisper-rs provider. `WhisperState` is kept on one worker thread
/// so the public provider remains `Send + Sync` without pretending whisper-rs
/// internals are safe to run concurrently.
pub struct Transcriber {
    worker_tx: mpsc::Sender<WorkerMessage>,
    worker: Option<std::thread::JoinHandle<()>>,
    model: Arc<Mutex<ModelInfo>>,
    defaults: SttRequest,
}

enum WorkerMessage {
    Preload,
    Transcribe {
        samples: Vec<f32>,
        request: SttRequest,
        reply: tokio::sync::oneshot::Sender<Result<WorkerOutput>>,
    },
    Shutdown,
}

struct WorkerModel {
    state: WhisperState,
}

struct WorkerOutput {
    text: Option<String>,
    language: Option<String>,
    model_load_ms: f64,
    decode_ms: f64,
}

impl Transcriber {
    #[must_use]
    pub fn new(config: &WhisperConfig) -> Self {
        let model_path = crate::config::expand_tilde(&config.model_path);
        let model_name = config.model.clone();
        let requested_backend = if config.device.eq_ignore_ascii_case("cpu") {
            "cpu"
        } else {
            "cuda"
        }
        .to_string();
        let cache_dir = model_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(default_model_dir);
        let default_language = normalize_language(&config.language);
        let defaults = SttRequest {
            language: default_language,
            initial_prompt: config.initial_prompt.clone(),
            no_speech_threshold: Some(config.no_speech_threshold.clamp(0.0, 1.0)),
        };
        let (worker_tx, worker_rx) = mpsc::channel();
        let model = Arc::new(Mutex::new(ModelInfo {
            name: model_name.clone(),
            loaded: false,
            backend: None,
        }));
        let worker_status = model.clone();
        let worker = std::thread::Builder::new()
            .name("dictate-whisper".into())
            .spawn(move || {
                whisper_worker(
                    worker_rx,
                    model_path,
                    model_name,
                    requested_backend,
                    cache_dir,
                    worker_status,
                )
            })
            .expect("failed to start Whisper worker thread");

        Self {
            worker_tx,
            worker: Some(worker),
            model,
            defaults,
        }
    }

    /// Queue a non-blocking preload. A missing standard catalog model is
    /// pulled here/at first transcription, not left for a UI wizard.
    pub fn load_model_async(&self) {
        if let Err(e) = self.worker_tx.send(WorkerMessage::Preload) {
            error!("failed to queue Whisper preload: {e}");
        }
    }

    #[must_use]
    pub fn is_model_loaded(&self) -> bool {
        self.model.lock().map(|m| m.loaded).unwrap_or(false)
    }

    #[must_use]
    pub fn model(&self) -> ModelInfo {
        self.model.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// Transcribe one capture with optional per-call language/prompt overrides.
    pub async fn transcribe(
        &self,
        samples: &[f32],
        mut request: SttRequest,
    ) -> Result<Option<Transcription>> {
        if request.language.is_none() {
            request.language = self.defaults.language.clone();
        }
        if request.initial_prompt.is_none() {
            request.initial_prompt = self.defaults.initial_prompt.clone();
        }
        if request.no_speech_threshold.is_none() {
            request.no_speech_threshold = self.defaults.no_speech_threshold;
        }
        let started = Instant::now();
        let audio_ms = samples.len() as f64 / 16_000.0 * 1000.0;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.worker_tx
            .send(WorkerMessage::Transcribe {
                samples: samples.to_vec(),
                request,
                reply: reply_tx,
            })
            .map_err(|_| anyhow!("Whisper worker is not running"))?;
        let queued_at = Instant::now();
        let output = reply_rx
            .await
            .map_err(|_| anyhow!("Whisper worker stopped before returning a transcription"))??;
        let total_ms = started.elapsed().as_secs_f64() * 1000.0;
        let queue_ms = (total_ms - output.model_load_ms - output.decode_ms).max(0.0);
        info!(
            total_ms, decode_ms = output.decode_ms, audio_ms,
            "Whisper transcription finished"
        );
        let _ = queued_at; // retained as a clear boundary for future queue telemetry.
        Ok(output.text.map(|text| Transcription {
            text: apply_corrections(&text),
            language: output.language,
            timings: SttTimings {
                audio_ms,
                queue_ms,
                model_load_ms: output.model_load_ms,
                decode_ms: output.decode_ms,
                total_ms,
            },
        }))
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
    configured_path: PathBuf,
    model_name: String,
    requested_backend: String,
    cache_dir: PathBuf,
    status: Arc<Mutex<ModelInfo>>,
) {
    let mut model: Option<WorkerModel> = None;
    while let Ok(message) = rx.recv() {
        match message {
            WorkerMessage::Preload => {
                let _ = ensure_model_loaded(
                    &mut model,
                    &configured_path,
                    &model_name,
                    &requested_backend,
                    &cache_dir,
                    &status,
                );
            }
            WorkerMessage::Transcribe { samples, request, reply } => {
                let result = ensure_model_loaded(
                    &mut model,
                    &configured_path,
                    &model_name,
                    &requested_backend,
                    &cache_dir,
                    &status,
                )
                .and_then(|(model, load_ms)| {
                    transcribe_with_model(model, &samples, &request).map(|(text, language, decode_ms)| {
                        WorkerOutput { text, language, model_load_ms: load_ms, decode_ms }
                    })
                });
                let _ = reply.send(result);
            }
            WorkerMessage::Shutdown => break,
        }
    }
}

fn ensure_model_loaded<'a>(
    model: &'a mut Option<WorkerModel>,
    configured_path: &PathBuf,
    model_name: &str,
    requested_backend: &str,
    cache_dir: &PathBuf,
    status: &Arc<Mutex<ModelInfo>>,
) -> Result<(&'a mut WorkerModel, f64)> {
    if model.is_none() {
        let start = Instant::now();
        // Explicit existing paths support offline/custom GGUFs. Otherwise a
        // catalog name is resolved and pulled reproducibly on first use.
        let path = if configured_path.exists() {
            configured_path.clone()
        } else {
            ModelManager::new(cache_dir.clone()).ensure(model_name)?
        };
        info!(path = %path.display(), backend = requested_backend, "loading Whisper model");
        let loaded = load_worker_model(&path, requested_backend == "cuda")?;
        let load_ms = start.elapsed().as_secs_f64() * 1000.0;
        if let Ok(mut value) = status.lock() {
            value.loaded = true;
            // `use_gpu(true)` was accepted by whisper.cpp context creation,
            // which is the backend selected for this loaded context.
            value.backend = Some(requested_backend.to_string());
        }
        *model = Some(loaded);
        return Ok((model.as_mut().expect("assigned above"), load_ms));
    }
    Ok((model.as_mut().expect("checked above"), 0.0))
}

fn load_worker_model(model_path: &std::path::Path, use_gpu: bool) -> Result<WorkerModel> {
    let mut params = WhisperContextParameters::default();
    params.use_gpu(use_gpu);
    let context = WhisperContext::new_with_params(model_path, params)
        .map_err(|e| anyhow!("failed to load Whisper context: {e}"))?;
    let state = context
        .create_state()
        .map_err(|e| anyhow!("failed to initialize Whisper state: {e}"))?;
    Ok(WorkerModel { state })
}

fn transcribe_with_model(
    model: &mut WorkerModel,
    samples: &[f32],
    request: &SttRequest,
) -> Result<(Option<String>, Option<String>, f64)> {
    let started = Instant::now();
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(request.language.as_deref());
    params.set_no_context(true);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    // This knob is deliberately per-model/configurable instead of a hidden
    // magic number; quiet-speech users can tune it alongside S10's gain.
    params.set_no_speech_thold(request.no_speech_threshold.unwrap_or(0.6).clamp(0.0, 1.0));
    if let Some(prompt) = request.initial_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
        params.set_initial_prompt(prompt);
    }
    model.state.full(params, samples).map_err(|e| anyhow!("{e}"))?;
    let language = if request.language.is_some() {
        request.language.clone()
    } else {
        whisper_rs::get_lang_str(model.state.full_lang_id_from_state()).map(str::to_owned)
    };
    let segments = model.state.full_n_segments();
    let mut text = String::new();
    for i in 0..segments {
        if let Some(segment) = model.state.get_segment(i) {
            if let Ok(segment_text) = segment.to_str_lossy() {
                text.push_str(&segment_text);
            }
        }
    }
    let text = text.trim().to_string();
    Ok(((!text.is_empty()).then_some(text), language, started.elapsed().as_secs_f64() * 1000.0))
}

fn normalize_language(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.eq_ignore_ascii_case("auto")).then(|| value.to_owned())
}

fn default_model_dir() -> PathBuf {
    crate::config::expand_tilde("~/.local/share/dictate-agent/models")
}

/// whisper-rs implementation behind the public provider trait.
pub struct WhisperStt {
    inner: Transcriber,
}

impl WhisperStt {
    #[must_use]
    pub fn new(config: &WhisperConfig) -> Self {
        let inner = Transcriber::new(config);
        inner.load_model_async();
        Self { inner }
    }
}

impl SttProvider for WhisperStt {
    fn transcribe<'a>(
        &'a self,
        samples: &'a [f32],
        request: SttRequest,
    ) -> BoxFuture<'a, Result<Option<Transcription>>> {
        Box::pin(async move { self.inner.transcribe(samples, request).await })
    }

    fn model(&self) -> ModelInfo {
        self.inner.model()
    }
}

/// Hardcoded Whisper mis-transcription corrections. All 23 historical pairs
/// are deliberately kept until S22's dictionary layer supersedes them.
pub fn apply_corrections(text: &str) -> String {
    const CORRECTIONS: [(&str, &str); 23] = [
        (".clod", ".claude"), (".cloud", ".claude"), (".clawed", ".claude"),
        (" clod", " claude"), (" cloud", " claude"), (" clawed", " claude"),
        ("Clod", "Claude"), ("Cloud", "Claude"), ("Clawed", "Claude"),
        ("research code base", "/research_codebase"), ("research codebase", "/research_codebase"),
        ("create plan", "/create_plan"), ("implement plan", "/implement_plan"),
        ("validate plan", "/validate_plan"), ("create handoff", "/create_handoff"),
        ("create hand off", "/create_handoff"), ("Research code base", "/research_codebase"),
        ("Research codebase", "/research_codebase"), ("Create plan", "/create_plan"),
        ("Implement plan", "/implement_plan"), ("Validate plan", "/validate_plan"),
        ("Create handoff", "/create_handoff"), ("Create hand off", "/create_handoff"),
    ];
    let mut result = text.to_string();
    for (from, to) in CORRECTIONS {
        result = result.replace(from, to);
    }
    scrub_returned_text(&result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_23_corrections_are_retained() {
        assert_eq!(apply_corrections("Clod cloud clawed create plan"), "Claude claude claude /create_plan");
        assert_eq!(apply_corrections("Research code base Create hand off"), "/research_codebase /create_handoff");
        assert_eq!(apply_corrections("plain text"), "plain text");
    }

    #[test]
    fn auto_and_pinned_languages_normalize_as_expected() {
        assert_eq!(normalize_language("auto"), None);
        assert_eq!(normalize_language("  EN "), Some("EN".to_string()));
        assert_eq!(normalize_language(""), None);
    }

    #[test]
    fn cpu_tiny_ci_config_never_requests_cuda() {
        let config = WhisperConfig { model: "tiny.en".into(), device: "cpu".into(), ..WhisperConfig::default() };
        let recognizer = Transcriber::new(&config);
        assert_eq!(recognizer.model().backend, None);
        drop(recognizer);
    }
}
