use anyhow::Result;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub success: bool,
    pub response: String,
    pub error: Option<String>,
}

pub struct LocalExecutor {
    host: String,
    port: u16,
    model: String,
    timeout: Duration,
}

impl LocalExecutor {
    pub fn new(config: &crate::config::LocalConfig) -> Self {
        let (host, port) = crate::grammar::parse_host_port(&config.host);
        Self {
            host,
            port,
            model: config.model.clone(),
            timeout: Duration::from_secs_f64(config.timeout_s),
        }
    }

    /// Execute a prompt against the local Ollama model.
    /// NEVER raises — returns ExecutionResult with success=false on any error.
    /// Port of local_executor.py execute().
    pub async fn execute(&self, prompt: &str, model_override: Option<&str>) -> ExecutionResult {
        let model = model_override.unwrap_or(&self.model).to_string();
        let start = Instant::now();

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
                }
            }
            Err(e) => {
                let msg = classify_error(&e);
                error!("Local execution failed: {}", msg);
                ExecutionResult {
                    success: false,
                    response: String::new(),
                    error: Some(msg),
                }
            }
        }
    }

    async fn call_ollama(&self, prompt: &str, model: &str) -> Result<String> {
        use ollama_rs::generation::completion::request::GenerationRequest;
        use ollama_rs::models::ModelOptions;
        use ollama_rs::Ollama;

        let ollama = Ollama::new(&self.host, self.port);
        let request = GenerationRequest::new(model.to_string(), prompt.to_string())
            .options(ModelOptions::default().num_predict(2048));

        let response = tokio::time::timeout(self.timeout, ollama.generate(request)).await??;

        Ok(response.response.trim().to_string())
    }
}

/// Check if Ollama is running by hitting /api/tags.
/// Port of local_executor.py:114-122
pub async fn is_ollama_running(host: &str, port: u16) -> bool {
    let ollama = ollama_rs::Ollama::new(host, port);
    match tokio::time::timeout(Duration::from_secs(2), ollama.list_local_models()).await {
        Ok(Ok(_)) => true,
        _ => false,
    }
}

/// Start Ollama if not running. Poll until ready or timeout.
/// Port of local_executor.py:124-168
pub async fn ensure_ollama_running(host: &str, port: u16, max_wait_s: u64) -> bool {
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

    #[test]
    fn test_classify_error_generic() {
        let err = anyhow::anyhow!("timeout after 120s");
        assert_eq!(classify_error(&err), "timeout after 120s");
    }
}
