//! Thin Ollama client behind the [`ChatBackend`] seam.
//!
//! # Why not `ollama-rs`
//!
//! `ollama-rs` is already a dependency (the legacy grammar pass and the LOCAL
//! route use it), but this layer needs three things it hides:
//!
//! - **the HTTP status and Ollama's error body**, to tell "model not found"
//!   (re-resolve the ladder) from "server down" (back off) from anything else —
//!   `ollama-rs` folds a non-2xx into an untyped `Other(String)`;
//! - **`done_reason`**, to reject output truncated by `num_predict` — its
//!   response type drops it;
//! - **a load-only request** (`messages: []`) for warm-up, whose reply has no
//!   token statistics and fails `ollama-rs`'s strict response type.
//!
//! `reqwest` is already in the dependency graph (through `ollama-rs` and
//! `dictate-stt`), so this costs no new crates. The request/response types
//! below are the documented `/api/chat` and `/api/tags` shapes with every
//! response field defaulted, so an Ollama upgrade that adds or drops a
//! statistic cannot turn into a parse failure.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Boxed `Send` future, so the backend trait stays object safe without an
/// `async-trait` dependency.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

/// Sampling options sent with every formatting request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatOptions {
    pub temperature: f32,
    /// Upper bound on generated tokens, derived from the input length.
    pub num_predict: i32,
    pub stop: Vec<String>,
    /// Fixed so a recording can be reproduced.
    pub seed: i32,
}

/// `POST /api/chat` body. `stream` and `think` are always false.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    pub think: bool,
    /// Ollama duration string or integer seconds; see [`keep_alive_value`].
    #[serde(serialize_with = "serialize_keep_alive")]
    pub keep_alive: String,
    pub options: ChatOptions,
}

/// Ollama accepts `keep_alive` as a number of seconds or a duration string;
/// a bare `"-1"` string is rejected by its duration parser, so unit-less
/// values go out as numbers.
#[must_use]
pub fn keep_alive_value(keep_alive: &str) -> serde_json::Value {
    let trimmed = keep_alive.trim();
    match trimmed.parse::<i64>() {
        Ok(n) => serde_json::Value::from(n),
        Err(_) => serde_json::Value::from(trimmed),
    }
}

fn serialize_keep_alive<S: serde::Serializer>(v: &str, s: S) -> Result<S::Ok, S::Error> {
    keep_alive_value(v).serialize(s)
}

/// The parts of an `/api/chat` reply this layer uses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub content: String,
    /// `"stop"`, `"length"` (hit `num_predict`), `"load"`, …
    pub done_reason: Option<String>,
    pub prompt_eval_count: u64,
    /// Prompt tokens served from Ollama's KV cache (prefix reuse).
    pub prompt_eval_cached_count: u64,
    pub eval_count: u64,
    /// Model load time Ollama reported, in ms (non-zero means it was cold).
    pub load_ms: f64,
    pub prompt_eval_ms: f64,
    pub eval_ms: f64,
}

/// An installed model from `/api/tags`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledModel {
    pub name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub size: u64,
}

impl InstalledModel {
    /// Embedding models are installed alongside chat models but cannot
    /// format text; they are never offered as alternatives.
    #[must_use]
    pub fn is_embedding(&self) -> bool {
        let n = self.name.to_ascii_lowercase();
        let f = self.family.to_ascii_lowercase();
        n.contains("embed") || f.contains("bert") || f.contains("embed")
    }
}

/// Why a backend call failed. Classified because each kind has a different
/// remedy: re-resolve the ladder, back off, or just fail open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// Connection refused / host unreachable: Ollama is not running.
    Unreachable(String),
    /// Ollama answered that the model is not installed.
    ModelMissing(String),
    /// The call exceeded its deadline.
    Timeout(Duration),
    /// Any other non-2xx answer.
    Http { status: u16, message: String },
    /// A 2xx whose body was not the expected JSON.
    Malformed(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(detail) => write!(f, "ollama unreachable: {detail}"),
            Self::ModelMissing(model) => write!(f, "model '{model}' not found"),
            Self::Timeout(d) => write!(f, "timed out after {} ms", d.as_millis()),
            Self::Http { status, message } => write!(f, "ollama HTTP {status}: {message}"),
            Self::Malformed(detail) => write!(f, "malformed ollama response: {detail}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// The Ollama operations the formatter needs. Implemented over HTTP by
/// [`HttpBackend`]; tests and the recorded eval tier substitute their own.
pub trait ChatBackend: Send + Sync {
    /// One non-streaming chat completion.
    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> BoxFuture<'a, Result<ChatResponse, BackendError>>;

    /// Installed models (`/api/tags`).
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<InstalledModel>, BackendError>>;

    /// Load `model` (or, with `keep_alive = "0"`, unload it) without
    /// generating. Returns the wall time the call took.
    fn load<'a>(
        &'a self,
        model: &'a str,
        keep_alive: &'a str,
    ) -> BoxFuture<'a, Result<Duration, BackendError>>;
}

/// Deadline for `/api/tags`: a local server answers in milliseconds, and the
/// probe runs on the dictation path after a failure.
const LIST_TIMEOUT: Duration = Duration::from_secs(2);
/// Deadline for a load-only request. Cold loads of a 3 GB model take ~3 s;
/// larger fallbacks take longer. Warm-up runs off the dictation path.
const LOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// [`ChatBackend`] over Ollama's HTTP API, with one pooled client.
#[derive(Debug, Clone)]
pub struct HttpBackend {
    base: String,
    client: reqwest::Client,
}

impl HttpBackend {
    /// `host` as configured: `http://localhost:11434`, `localhost:11434`, …
    #[must_use]
    pub fn new(host: &str) -> Self {
        let client = reqwest::Client::builder()
            // Local server: a connect that takes longer than this is a
            // server that is not there.
            .connect_timeout(Duration::from_secs(1))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base: normalize_base(host),
            client,
        }
    }

    /// The normalized base URL, for messages.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base
    }

    async fn post_chat(&self, body: serde_json::Value) -> Result<ChatResponse, BackendError> {
        let model = body
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let response = self
            .client
            .post(format!("{}/api/chat", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| classify_transport(&e, &self.base))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| classify_transport(&e, &self.base))?;
        if !status.is_success() {
            return Err(classify_status(status.as_u16(), &bytes, &model));
        }
        parse_chat(&bytes)
    }
}

impl ChatBackend for HttpBackend {
    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> BoxFuture<'a, Result<ChatResponse, BackendError>> {
        Box::pin(async move {
            let body = serde_json::to_value(request)
                .map_err(|e| BackendError::Malformed(format!("request encoding: {e}")))?;
            self.post_chat(body).await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<InstalledModel>, BackendError>> {
        Box::pin(async move {
            let fut = async {
                let response = self
                    .client
                    .get(format!("{}/api/tags", self.base))
                    .send()
                    .await
                    .map_err(|e| classify_transport(&e, &self.base))?;
                let status = response.status();
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|e| classify_transport(&e, &self.base))?;
                if !status.is_success() {
                    return Err(classify_status(status.as_u16(), &bytes, ""));
                }
                parse_tags(&bytes)
            };
            tokio::time::timeout(LIST_TIMEOUT, fut)
                .await
                .map_err(|_| BackendError::Timeout(LIST_TIMEOUT))?
        })
    }

    fn load<'a>(
        &'a self,
        model: &'a str,
        keep_alive: &'a str,
    ) -> BoxFuture<'a, Result<Duration, BackendError>> {
        Box::pin(async move {
            let started = Instant::now();
            let body = serde_json::json!({
                "model": model,
                "messages": [],
                "stream": false,
                "keep_alive": keep_alive_value(keep_alive),
            });
            tokio::time::timeout(LOAD_TIMEOUT, self.post_chat(body))
                .await
                .map_err(|_| BackendError::Timeout(LOAD_TIMEOUT))??;
            Ok(started.elapsed())
        })
    }
}

/// `localhost:11434` → `http://localhost:11434`; trailing slashes dropped.
#[must_use]
pub fn normalize_base(host: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    if host.contains("://") {
        host.to_string()
    } else {
        format!("http://{host}")
    }
}

fn classify_transport(e: &reqwest::Error, base: &str) -> BackendError {
    if e.is_timeout() {
        // reqwest's own connect timeout; request deadlines are applied by
        // the caller and surface as `Timeout` there.
        BackendError::Unreachable(format!("{base}: connect timed out"))
    } else if e.is_connect() {
        BackendError::Unreachable(format!("{base}: connection refused"))
    } else if e.is_decode() || e.is_body() {
        BackendError::Malformed(e.to_string())
    } else {
        BackendError::Unreachable(format!("{base}: {e}"))
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
}

fn classify_status(status: u16, body: &[u8], model: &str) -> BackendError {
    let message = serde_json::from_slice::<ErrorBody>(body)
        .map(|b| b.error)
        .unwrap_or_else(|_| String::from_utf8_lossy(body).chars().take(200).collect());
    // Ollama answers 404 `{"error":"model 'x' not found"}` for a missing
    // model; older servers used other codes with the same message.
    let lower = message.to_ascii_lowercase();
    if lower.contains("not found") && (status == 404 || lower.contains("model")) {
        let name = if model.is_empty() {
            message.clone()
        } else {
            model.to_string()
        };
        return BackendError::ModelMissing(name);
    }
    BackendError::Http { status, message }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireChat {
    message: Option<WireMessage>,
    done_reason: Option<String>,
    prompt_eval_count: u64,
    prompt_eval_cached_count: u64,
    eval_count: u64,
    load_duration: u64,
    prompt_eval_duration: u64,
    eval_duration: u64,
    error: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireMessage {
    content: String,
}

fn parse_chat(bytes: &[u8]) -> Result<ChatResponse, BackendError> {
    let wire: WireChat = serde_json::from_slice(bytes).map_err(|e| {
        let snippet: String = String::from_utf8_lossy(bytes).chars().take(80).collect();
        BackendError::Malformed(format!("{e} (body starts {snippet:?})"))
    })?;
    if let Some(error) = wire.error {
        return Err(BackendError::Http {
            status: 200,
            message: error,
        });
    }
    let ns_to_ms = |ns: u64| ns as f64 / 1e6;
    Ok(ChatResponse {
        content: wire.message.map(|m| m.content).unwrap_or_default(),
        done_reason: wire.done_reason,
        prompt_eval_count: wire.prompt_eval_count,
        prompt_eval_cached_count: wire.prompt_eval_cached_count,
        eval_count: wire.eval_count,
        load_ms: ns_to_ms(wire.load_duration),
        prompt_eval_ms: ns_to_ms(wire.prompt_eval_duration),
        eval_ms: ns_to_ms(wire.eval_duration),
    })
}

#[derive(Deserialize)]
struct WireTags {
    models: Vec<WireTag>,
}

#[derive(Deserialize)]
struct WireTag {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    details: WireDetails,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireDetails {
    family: String,
}

fn parse_tags(bytes: &[u8]) -> Result<Vec<InstalledModel>, BackendError> {
    let tags: WireTags =
        serde_json::from_slice(bytes).map_err(|e| BackendError::Malformed(e.to_string()))?;
    Ok(tags
        .models
        .into_iter()
        .map(|t| InstalledModel {
            name: t.name,
            family: t.details.family,
            size: t.size,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_to_the_ollama_chat_shape() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![ChatMessage::system("s"), ChatMessage::user("u")],
            stream: false,
            think: false,
            keep_alive: "30m".into(),
            options: ChatOptions {
                temperature: 0.1,
                num_predict: 64,
                stop: vec!["</dictation>".into()],
                seed: 7,
            },
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["think"], false);
        assert_eq!(v["stream"], false);
        assert_eq!(v["keep_alive"], "30m");
        assert_eq!(v["options"]["num_predict"], 64);
        assert_eq!(v["options"]["stop"][0], "</dictation>");
        assert_eq!(v["messages"][1]["role"], "user");
    }

    #[test]
    fn unitless_keep_alive_goes_out_as_a_number() {
        assert_eq!(keep_alive_value("-1"), serde_json::json!(-1));
        assert_eq!(keep_alive_value("0"), serde_json::json!(0));
        assert_eq!(keep_alive_value("30m"), serde_json::json!("30m"));
    }

    #[test]
    fn base_url_normalization() {
        assert_eq!(
            normalize_base("http://localhost:11434/"),
            "http://localhost:11434"
        );
        assert_eq!(normalize_base("localhost:11434"), "http://localhost:11434");
        assert_eq!(normalize_base(" https://h:1 "), "https://h:1");
    }

    #[test]
    fn missing_model_is_classified_from_the_error_body() {
        let e = classify_status(
            404,
            br#"{"error":"model 'qwen3:14b' not found"}"#,
            "qwen3:14b",
        );
        assert_eq!(e, BackendError::ModelMissing("qwen3:14b".into()));
        let other = classify_status(500, br#"{"error":"out of memory"}"#, "m");
        assert_eq!(
            other,
            BackendError::Http {
                status: 500,
                message: "out of memory".into()
            }
        );
        // A non-JSON error body is kept (truncated) rather than lost.
        let raw = classify_status(502, b"bad gateway", "m");
        assert!(
            matches!(raw, BackendError::Http { status: 502, ref message } if message == "bad gateway")
        );
    }

    #[test]
    fn chat_reply_parses_and_tolerates_missing_statistics() {
        let r = parse_chat(
            br#"{"model":"m","message":{"role":"assistant","content":"Hi."},"done":true,"done_reason":"stop","eval_count":3,"eval_duration":21000000}"#,
        )
        .unwrap();
        assert_eq!(r.content, "Hi.");
        assert_eq!(r.done_reason.as_deref(), Some("stop"));
        assert_eq!(r.eval_count, 3);
        assert!((r.eval_ms - 21.0).abs() < 1e-9);
        assert_eq!(r.prompt_eval_count, 0);
        // A load-only reply has no message at all.
        let load = parse_chat(br#"{"model":"m","done":true,"done_reason":"load"}"#).unwrap();
        assert_eq!(load.content, "");
    }

    #[test]
    fn malformed_and_in_band_errors_are_errors() {
        assert!(matches!(
            parse_chat(b"not json"),
            Err(BackendError::Malformed(_))
        ));
        assert!(matches!(
            parse_chat(br#"{"error":"boom"}"#),
            Err(BackendError::Http { status: 200, .. })
        ));
    }

    #[test]
    fn tags_parse_and_embedding_models_are_recognized() {
        let models = parse_tags(
            br#"{"models":[{"name":"gemma4:e4b","size":1,"details":{"family":"gemma4"}},{"name":"qwen3-embedding:0.6b","details":{"family":"qwen3"}}]}"#,
        )
        .unwrap();
        assert_eq!(models.len(), 2);
        assert!(!models[0].is_embedding());
        assert!(models[1].is_embedding());
    }
}
