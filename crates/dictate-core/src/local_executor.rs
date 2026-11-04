use anyhow::Result;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use crate::ollama_host::{ManagedUnit, OllamaHost};

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
        let (host, port) = dictate_fmt::llm::parse_host_port(&config.host);
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

        Ok(clean_answer(&response.response))
    }
}

/// What the user sees of a LOCAL answer: the model's text minus model
/// artifacts. Never strips a "thank you." (#1069, #1072).
fn clean_answer(raw: &str) -> String {
    scrub_returned_text(raw)
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

/// Starts `ollama serve`. A trait so tests can prove a server was (not) started
/// without running a real one.
pub trait OllamaSpawner: Send + Sync {
    /// Start the server and return without waiting for it.
    ///
    /// # Errors
    ///
    /// When the process cannot be started.
    fn spawn_serve(&self) -> std::io::Result<()>;
}

/// The production spawner: a bare `ollama serve` on the user's environment.
pub struct ProcessSpawner;

impl OllamaSpawner for ProcessSpawner {
    fn spawn_serve(&self) -> std::io::Result<()> {
        tokio::process::Command::new("ollama")
            .arg("serve")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
    }
}

/// What [`ensure_ollama`] found, and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OllamaStartup {
    /// The configured host is not a usable URL; nothing was tried.
    InvalidHost(String),
    /// A server answers at the endpoint. dictated did not start it.
    Up,
    /// Something holds the port but does not answer `/api/tags`.
    Busy,
    /// Nothing answers, and a managed `ollama.service` exists: it is down.
    /// dictated does not start a second server beside it.
    ManagedDown(ManagedUnit),
    /// Nothing answers and the host is another machine; a local server would
    /// not help.
    NotLocal,
    /// No unit and nothing on the port: dictated started `ollama serve`, and
    /// it answers.
    Spawned,
    /// As [`Spawned`](Self::Spawned), but it did not answer in time.
    SpawnedNotReady,
    /// `ollama serve` could not be started.
    SpawnFailed(String),
}

impl OllamaStartup {
    /// Whether dictated started a server.
    #[must_use]
    pub fn spawned(&self) -> bool {
        matches!(self, Self::Spawned | Self::SpawnedNotReady)
    }

    /// Say what happened, once, at the right level.
    pub fn log(&self, host: &str, port: u16) {
        match self {
            Self::InvalidHost(why) => warn!("not starting Ollama: host {why}"),
            Self::Up => info!("Ollama answers at {host}:{port}; dictated did not start it"),
            Self::Busy => warn!(
                "something holds {host}:{port} but does not answer Ollama's /api/tags; \
                 dictated will not start a second server (the formatter fails open until it answers)"
            ),
            Self::ManagedDown(unit) => warn!(
                "Ollama is down at {host}:{port} and {} exists, so dictated will not start its own \
                 server (it would read a different model store). Run `{}`; the formatter fails open \
                 and retries on its own",
                unit.path.display(),
                unit.start_command()
            ),
            Self::NotLocal => warn!(
                "Ollama does not answer at {host}:{port}, which is not this machine; not starting a local server"
            ),
            Self::Spawned => info!("Ollama server started by dictated and ready"),
            Self::SpawnedNotReady => warn!("Ollama server started by dictated did not answer in time"),
            Self::SpawnFailed(e) => error!("Failed to start Ollama: {e}"),
        }
    }
}

/// Make sure an Ollama answers at `host:port`, starting one only when nothing
/// else could (#5725).
///
/// dictated never owns the endpoint when a managed Ollama exists: if the port
/// answers, or is bound, or an `ollama.service` unit exists, it waits up to
/// `wait` for the endpoint to answer and otherwise reports it and fails open.
/// Only with no unit, nothing on the port and a local host does it run
/// `ollama serve` through `spawner`.
pub async fn ensure_ollama(
    host: &str,
    port: u16,
    wait: Duration,
    ollama_host: &OllamaHost,
    spawner: &dyn OllamaSpawner,
) -> OllamaStartup {
    let endpoint_host = match crate::ollama::client_at(host, port) {
        Ok(client) => client
            .url()
            .host_str()
            .unwrap_or_default()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string(),
        Err(why) => return OllamaStartup::InvalidHost(why),
    };
    if is_ollama_running(host, port).await {
        return OllamaStartup::Up;
    }
    let unit = ollama_host.managed_unit();
    let bound = endpoint_bound(&endpoint_host, port).await;

    if unit.is_some() || bound {
        // Not ours to start. The unit may simply be a moment behind us at boot.
        if wait_until_answering(host, port, wait).await {
            return OllamaStartup::Up;
        }
        return match unit {
            Some(unit) if !bound => OllamaStartup::ManagedDown(unit),
            _ => OllamaStartup::Busy,
        };
    }
    if !is_loopback(&endpoint_host) {
        return OllamaStartup::NotLocal;
    }

    info!("Starting Ollama server (no ollama.service exists and nothing holds the port)...");
    match spawner.spawn_serve() {
        Ok(()) => {
            if wait_until_answering(host, port, wait).await {
                OllamaStartup::Spawned
            } else {
                OllamaStartup::SpawnedNotReady
            }
        }
        Err(e) => OllamaStartup::SpawnFailed(e.to_string()),
    }
}

/// Poll `/api/tags` until it answers or `wait` passes (one try when zero).
async fn wait_until_answering(host: &str, port: u16, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if is_ollama_running(host, port).await {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        tokio::time::sleep(left.min(Duration::from_millis(500))).await;
    }
}

/// Whether something accepts a TCP connection at the endpoint.
async fn endpoint_bound(host: &str, port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::net::TcpStream::connect((host, port))
        )
        .await,
        Ok(Ok(_))
    )
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
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
    fn an_answer_keeps_its_thank_you() {
        for text in [
            "I just wanted to thank you.",
            "Thanks for the report. Thank you.",
        ] {
            assert_eq!(clean_answer(text), text);
        }
        assert_eq!(
            clean_answer("Done. Thank you. /no_think"),
            "Done. Thank you."
        );
    }

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

/// #5725: dictated must never become the owner of the Ollama endpoint when a
/// managed Ollama exists. Every test uses a private ephemeral port and a
/// private unit directory; none touches 11434 or a real unit.
#[cfg(test)]
mod startup_tests {
    use super::*;
    use crate::ollama_host::testing::Scratch;
    use crate::ollama_host::UnitScope;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Default)]
    struct CountingSpawner {
        calls: AtomicUsize,
        fail: bool,
    }

    impl OllamaSpawner for CountingSpawner {
        fn spawn_serve(&self) -> std::io::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(std::io::Error::other("no such program"))
            } else {
                Ok(())
            }
        }
    }

    impl CountingSpawner {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    /// A port nothing listens on right now.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// A fake Ollama answering `/api/tags` with `models` on `listener`.
    fn serve_fake(listener: tokio::net::TcpListener, models: &'static [&'static str]) {
        let body = serde_json::json!({
            "models": models.iter().map(|m| serde_json::json!({
                "name": m, "modified_at": "2026-01-01T00:00:00Z", "size": 1,
            })).collect::<Vec<_>>()
        })
        .to_string();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
    }

    async fn fake_ollama(models: &'static [&'static str]) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        serve_fake(listener, models);
        port
    }

    fn host_with_unit(dir: &Scratch) -> OllamaHost {
        dir.write(
            "units/ollama.service",
            "[Service]\nExecStart=/usr/bin/ollama serve\n",
        );
        OllamaHost::default().with_unit_dir(dir.path("units"), UnitScope::System)
    }

    const LOCAL: &str = "http://127.0.0.1";

    #[tokio::test]
    async fn a_managed_unit_that_is_down_is_reported_and_no_server_is_spawned() {
        let dir = Scratch::new("managed");
        let spawner = CountingSpawner::default();
        let outcome = ensure_ollama(
            LOCAL,
            free_port(),
            Duration::ZERO,
            &host_with_unit(&dir),
            &spawner,
        )
        .await;
        match outcome {
            OllamaStartup::ManagedDown(unit) => {
                assert_eq!(unit.start_command(), "sudo systemctl start ollama");
            }
            other => panic!("expected ManagedDown, got {other:?}"),
        }
        assert_eq!(spawner.calls(), 0, "dictated must not start ollama serve");
    }

    #[tokio::test]
    async fn a_drop_in_directory_alone_still_counts_as_a_managed_unit() {
        let dir = Scratch::new("dropin-only");
        dir.write("units/ollama.service.d/10-models.conf", "[Service]\n");
        let host = OllamaHost::default().with_unit_dir(dir.path("units"), UnitScope::System);
        let spawner = CountingSpawner::default();
        let outcome = ensure_ollama(LOCAL, free_port(), Duration::ZERO, &host, &spawner).await;
        assert!(
            matches!(outcome, OllamaStartup::ManagedDown(_)),
            "{outcome:?}"
        );
        assert_eq!(spawner.calls(), 0);
    }

    #[tokio::test]
    async fn an_answering_endpoint_is_never_replaced_whether_or_not_a_unit_exists() {
        let dir = Scratch::new("answering");
        let port = fake_ollama(&["gemma4:e4b"]).await;
        for host in [OllamaHost::default(), host_with_unit(&dir)] {
            let spawner = CountingSpawner::default();
            let outcome = ensure_ollama(LOCAL, port, Duration::ZERO, &host, &spawner).await;
            assert_eq!(outcome, OllamaStartup::Up);
            assert_eq!(spawner.calls(), 0);
        }
    }

    #[tokio::test]
    async fn a_bound_port_that_does_not_answer_is_left_alone() {
        // Accepts and hangs up: bound, but not an Ollama.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { while listener.accept().await.is_ok() {} });
        let spawner = CountingSpawner::default();
        let outcome = ensure_ollama(
            LOCAL,
            port,
            Duration::ZERO,
            &OllamaHost::default(),
            &spawner,
        )
        .await;
        assert_eq!(outcome, OllamaStartup::Busy);
        assert_eq!(spawner.calls(), 0, "no unit, but the port is taken");
    }

    #[tokio::test]
    async fn a_unit_that_comes_up_during_the_wait_is_used_not_replaced() {
        let dir = Scratch::new("late");
        let port = free_port();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .unwrap();
            serve_fake(listener, &["gemma4:e4b"]);
        });
        let spawner = CountingSpawner::default();
        let outcome = ensure_ollama(
            LOCAL,
            port,
            Duration::from_secs(5),
            &host_with_unit(&dir),
            &spawner,
        )
        .await;
        assert_eq!(outcome, OllamaStartup::Up);
        assert_eq!(spawner.calls(), 0);
    }

    #[tokio::test]
    async fn with_no_unit_and_no_server_the_old_behaviour_is_kept() {
        let spawner = Arc::new(CountingSpawner::default());
        let outcome = ensure_ollama(
            LOCAL,
            free_port(),
            Duration::ZERO,
            &OllamaHost::default(),
            spawner.as_ref(),
        )
        .await;
        assert_eq!(outcome, OllamaStartup::SpawnedNotReady);
        assert!(outcome.spawned());
        assert_eq!(spawner.calls(), 1);

        let failing = CountingSpawner {
            fail: true,
            ..CountingSpawner::default()
        };
        let outcome = ensure_ollama(
            LOCAL,
            free_port(),
            Duration::ZERO,
            &OllamaHost::default(),
            &failing,
        )
        .await;
        assert!(
            matches!(outcome, OllamaStartup::SpawnFailed(_)),
            "{outcome:?}"
        );
        assert_eq!(failing.calls(), 1);
    }

    #[tokio::test]
    async fn a_malformed_host_spawns_nothing() {
        let spawner = CountingSpawner::default();
        let outcome = ensure_ollama(
            "not a url",
            1,
            Duration::ZERO,
            &OllamaHost::default(),
            &spawner,
        )
        .await;
        assert!(matches!(outcome, OllamaStartup::InvalidHost(_)));
        assert_eq!(spawner.calls(), 0);
    }

    #[test]
    fn only_a_local_host_can_be_served_by_a_local_spawn() {
        for local in ["127.0.0.1", "localhost", "LOCALHOST", "::1", "127.0.0.2"] {
            assert!(is_loopback(local), "{local}");
        }
        for remote in ["192.0.2.1", "ollama.example.com", "0.0.0.0", ""] {
            assert!(!is_loopback(remote), "{remote}");
        }
    }
}
