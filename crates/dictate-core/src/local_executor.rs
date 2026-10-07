use anyhow::Result;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use dictate_fmt::llm::{BackendError, HttpBackend, LlmHealth, ModelResolver};
use dictate_fmt::text_cleanup::scrub_returned_text;

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub success: bool,
    pub response: String,
    pub error: Option<String>,
    /// The model that was asked (resolved from the ladder), if one was.
    pub model: Option<String>,
}

/// Runs the LOCAL route against Ollama.
///
/// The configured `[local].model` is the first rung of a model ladder
/// (`[local].models` follows), resolved against the installed models
/// (`/api/tags`) on first use and again after a "model not found" — the same
/// mechanism as the formatting LLM (`dictate_fmt::llm::resolve`). Before this,
/// a removed `qwen3:14b` made every LOCAL request fail with nothing but a log
/// line.
pub struct LocalExecutor {
    host: String,
    port: u16,
    timeout: Duration,
    probe: HttpBackend,
    resolver: ModelResolver,
}

impl LocalExecutor {
    pub fn new(config: &crate::config::LocalConfig) -> Self {
        let (host, port) = dictate_fmt::grammar::parse_host_port(&config.host);
        let mut ladder = vec![config.model.clone()];
        for m in &config.models {
            if !ladder.contains(m) && !m.trim().is_empty() {
                ladder.push(m.clone());
            }
        }
        ladder.retain(|m| !m.trim().is_empty());
        Self {
            host,
            port,
            // Config loading refuses a timeout that does not convert; a
            // hand-built config that skipped it gets the default, not a panic.
            timeout: Duration::try_from_secs_f64(config.timeout_s)
                .unwrap_or(Duration::from_secs(120)),
            probe: HttpBackend::new(&config.host),
            resolver: ModelResolver::new("local", ladder),
        }
    }

    /// Current model health, without probing (for `status`/`doctor`).
    #[must_use]
    pub fn health(&self) -> LlmHealth {
        self.resolver.health()
    }

    /// Probe Ollama and resolve the ladder now.
    pub async fn refresh(&self) -> LlmHealth {
        self.resolver.refresh(&self.probe).await
    }

    /// Execute a prompt against the local Ollama model.
    /// NEVER raises — returns ExecutionResult with success=false on any error.
    /// Port of local_executor.py execute().
    pub async fn execute(&self, prompt: &str, model_override: Option<&str>) -> ExecutionResult {
        let start = Instant::now();
        let model = match model_override {
            Some(m) => m.to_string(),
            None => match self.resolver.ensure(&self.probe).await {
                Ok(m) => m,
                Err(reason) => {
                    let msg = format!("No local model available: {reason}");
                    error!("Local execution failed: {}", msg);
                    return ExecutionResult {
                        success: false,
                        response: String::new(),
                        error: Some(msg),
                        model: None,
                    };
                }
            },
        };

        match self.call_ollama(prompt, &model).await {
            Ok(response) => {
                info!(
                    "Local execution in {:.3}s ({} chars)",
                    start.elapsed().as_secs_f64(),
                    response.len()
                );
                ExecutionResult {
                    success: true,
                    response,
                    error: None,
                    model: Some(model),
                }
            }
            Err(e) => {
                if e.to_string().to_lowercase().contains("not found") {
                    // Re-resolve on the next request instead of failing forever.
                    self.resolver
                        .record_failure(&BackendError::ModelMissing(model.clone()));
                }
                let msg = classify_error(&e);
                error!("Local execution failed: {}", msg);
                ExecutionResult {
                    success: false,
                    response: String::new(),
                    error: Some(msg),
                    model: Some(model),
                }
            }
        }
    }

    async fn call_ollama(&self, prompt: &str, model: &str) -> Result<String> {
        use ollama_rs::generation::completion::request::GenerationRequest;
        use ollama_rs::models::ModelOptions;

        let ollama = crate::ollama::client_at(&self.host, self.port)
            .map_err(|why| anyhow::anyhow!("Ollama host {why}"))?;
        let request = GenerationRequest::new(model.to_string(), prompt.to_string())
            .options(ModelOptions::default().num_predict(2048));

        let response = tokio::time::timeout(self.timeout, ollama.generate(request)).await??;

        Ok(scrub_returned_text(&response.response))
    }
}

/// Check if Ollama is running by hitting /api/tags.
/// Port of local_executor.py:114-122
pub async fn is_ollama_running(host: &str, port: u16) -> bool {
    let Ok(ollama) = crate::ollama::client_at(host, port) else {
        return false;
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(2), ollama.list_local_models()).await,
        Ok(Ok(_))
    )
}

/// Start Ollama if not running. Poll until ready or timeout.
/// Port of local_executor.py:124-168
pub async fn ensure_ollama_running(host: &str, port: u16, max_wait_s: u64) -> bool {
    if let Err(why) = crate::ollama::client_at(host, port) {
        // Starting a server would not make a malformed host reachable.
        warn!("not starting Ollama: host {why}");
        return false;
    }
    if is_ollama_running(host, port).await {
        return true;
    }

    info!("Starting Ollama server...");
    match tokio::process::Command::new("ollama")
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => {
            let deadline = Instant::now() + Duration::from_secs(max_wait_s);
            while Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if is_ollama_running(host, port).await {
                    info!("Ollama server ready");
                    return true;
                }
            }
            warn!("Ollama server did not start within {}s", max_wait_s);
            false
        }
        Err(e) => {
            error!("Failed to start Ollama: {}", e);
            false
        }
    }
}

/// Classify Ollama errors into user-friendly messages.
/// Port of local_executor.py:78-81
fn classify_error(e: &anyhow::Error) -> String {
    let msg = e.to_string().to_lowercase();
    if msg.contains("connection") || msg.contains("refused") {
        "Ollama is not running. Start it with: ollama serve".into()
    } else if msg.contains("not found") {
        "Model not found. Pull it with: ollama pull <model>".into()
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_error_connection() {
        let err = anyhow::anyhow!("Connection refused");
        assert!(classify_error(&err).contains("not running"));
    }

    #[test]
    fn test_classify_error_not_found() {
        let err = anyhow::anyhow!("model 'test' not found");
        assert!(classify_error(&err).contains("Pull it"));
    }

    fn local_config(host: &str, model: &str) -> crate::config::LocalConfig {
        crate::config::LocalConfig {
            host: host.to_string(),
            model: model.to_string(),
            ..crate::config::LocalConfig::default()
        }
    }

    #[test]
    fn configured_model_heads_the_ladder_without_duplicates() {
        let mut c = local_config("http://127.0.0.1:9", "gemma4:12b");
        c.models = vec!["gemma4:12b".into(), "gemma4:e4b".into(), " ".into()];
        let e = LocalExecutor::new(&c);
        assert_eq!(e.resolver.ladder(), ["gemma4:12b", "gemma4:e4b"]);
        assert_eq!(e.health(), LlmHealth::Unchecked);
    }

    #[tokio::test]
    async fn ollama_down_is_reported_not_hidden() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let e = LocalExecutor::new(&local_config(
            &format!("http://127.0.0.1:{port}"),
            "qwen3:14b",
        ));
        let r = e.execute("what time is it", None).await;
        assert!(!r.success);
        assert_eq!(r.model, None);
        assert!(r.error.unwrap().contains("No local model available"));
        assert!(matches!(e.health(), LlmHealth::Unavailable { .. }));
    }

    #[test]
    fn test_classify_error_generic() {
        let err = anyhow::anyhow!("timeout after 120s");
        assert_eq!(classify_error(&err), "timeout after 120s");
    }
}
