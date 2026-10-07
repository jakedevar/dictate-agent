//! Routes and the admission gate.
//!
//! ```text
//! POST /v1/transcribe   audio body → `transcribe_audio` → transcript JSON
//! GET  /v1/status       `handshake` + `get_status` → status JSON
//! GET  /v1/ws           WebSocket: one dictate-proto envelope per text frame
//! ```
//!
//! [`gate`] runs before every handler, every WebSocket upgrade and the 404
//! fallback, in this order: `Host`, `Origin`, the per-peer throttle, the
//! bearer token. Nothing reaches the daemon before all four pass.
//!
//! Every refusal closes its connection (`Connection: close`), so a peer that
//! cannot get a request admitted pays a TCP handshake per attempt, and the
//! warnings refusals write share one budget ([`LogBudget`]).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use dictate_proto::{
    AudioEncoding, AudioFormat, AudioSource, ClientInfo, ClientKind, Command, CommandResult,
    ErrorCode, Hello, ProtoError, Route, SessionOptions,
};
use serde::Deserialize;
use tokio::sync::{watch, Semaphore};
use tracing::{debug, info, warn};

use crate::backend::{never, Backend, Session};
use crate::guard::{bearer, origin_allowed, AuthFailure, HostPolicy};
use crate::limit::{LogBudget, Throttle, Throttled};
use crate::token::{Credential, TokenStore};
use crate::ws::WsTasks;

/// The whole upload body must arrive within this.
const BODY_TIMEOUT: Duration = Duration::from_secs(60);

/// The TCP peer of a request, inserted by the accept loop. Never derived from
/// a header.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PeerAddr(pub SocketAddr);

/// What every request handler shares.
pub(crate) struct AppState {
    pub backend: Arc<dyn Backend>,
    pub tokens: Arc<TokenStore>,
    pub throttle: Arc<Throttle>,
    pub hosts: HostPolicy,
    pub origins: Vec<String>,
    pub max_upload_bytes: u64,
    /// The most a WebSocket buffers per message: [`Backend::max_message_bytes`].
    pub ws_message_limit: usize,
    pub ws_slots: Arc<Semaphore>,
    /// Upgraded WebSocket sessions, so shutdown can wait for them and abort
    /// what will not finish.
    pub ws_tasks: Arc<WsTasks>,
    /// Shared by every warning about a refused request.
    pub log_budget: LogBudget,
    /// Flips to `true` when the server shuts down, so WebSocket sessions
    /// (which outlive their HTTP connection) close too.
    pub closing: watch::Receiver<bool>,
}

pub(crate) fn router(state: Arc<AppState>) -> Router {
    let body_limit = usize::try_from(state.max_upload_bytes).unwrap_or(usize::MAX);
    Router::new()
        .route("/v1/transcribe", post(transcribe))
        .route("/v1/status", get(status))
        .route("/v1/ws", get(ws))
        .fallback(not_found)
        // `Router::layer` (not `route_layer`) so the fallback is gated too: an
        // unauthenticated peer cannot even map which paths exist.
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(middleware::from_fn_with_state(state.clone(), gate))
        .with_state(state)
}

/// A protocol error as an HTTP response: the error object, with the status
/// the protocol assigns its code.
pub(crate) fn error_response(error: &ProtoError) -> Response {
    let status =
        StatusCode::from_u16(error.code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    json_response(status, error)
}

fn json_response<T: serde::Serialize>(status: StatusCode, body: &T) -> Response {
    match serde_json::to_vec(body) {
        Ok(bytes) => (
            status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// A warning about a refused request, within [`AppState::log_budget`]. Past
/// the budget it is written at DEBUG instead: still there for an operator who
/// asks, never a flood in the journal.
macro_rules! refusal {
    ($state:expr, $($field:tt)+) => {
        match $state.log_budget.admit(Instant::now()) {
            Some(suppressed) => warn!(suppressed, $($field)+),
            None => debug!($($field)+),
        }
    };
}

/// Ask the client to go away after this response: a refused peer must
/// reconnect to try again rather than reuse a warm keep-alive connection.
fn close_after(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    response
}

/// Admission: `Host`, `Origin`, throttle, token — then the handler.
async fn gate(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let Some(PeerAddr(peer)) = request.extensions().get::<PeerAddr>().copied() else {
        // The accept loop always inserts it; without it there is no throttle
        // key, so refuse rather than guess.
        return error_response(&ProtoError::new(ErrorCode::Internal, "no peer address"));
    };
    let method = request.method().clone();
    // The path only: query strings can carry an `app` hint or a language, and
    // nothing about a request's parameters belongs in the journal.
    let path = request.uri().path().to_string();
    let headers = request.headers();

    if !state
        .hosts
        .allows(headers.get(header::HOST).and_then(|v| v.to_str().ok()))
    {
        refusal!(state, peer = %peer.ip(), %method, %path, "network API refused a request: Host not allowed");
        return close_after(json_response(
            StatusCode::MISDIRECTED_REQUEST,
            &ProtoError::new(
                ErrorCode::Forbidden,
                "this Host is not served here; add it to [api] allowed_hosts if it should be",
            ),
        ));
    }

    let origin = headers
        .get(header::ORIGIN)
        .map(|v| v.to_str().unwrap_or("\u{fffd}"));
    if !origin_allowed(origin, &state.origins) {
        refusal!(state, peer = %peer.ip(), %method, %path, "network API refused a browser request (Origin present)");
        return close_after(error_response(&ProtoError::new(
            ErrorCode::Forbidden,
            "requests from web pages are refused (Origin header present)",
        )));
    }

    if let Err(throttled) = state.throttle.check(peer.ip(), Instant::now()) {
        debug!(peer = %peer.ip(), %path, what = throttled.as_str(), "network API throttled a request");
        return close_after(throttled_response(throttled));
    }

    let verdict = match bearer(
        headers
            .get(header::AUTHORIZATION)
            .map(HeaderValue::as_bytes),
    ) {
        Ok(token) => state
            .tokens
            .authenticate(token)
            .ok_or(AuthFailure::Mismatch),
        Err(failure) => Err(failure),
    };
    let credential = match verdict {
        Ok(credential) => credential,
        Err(failure) => {
            let locked = state.throttle.record_failure(peer.ip(), Instant::now());
            refusal!(
                state,
                peer = %peer.ip(),
                %method,
                %path,
                reason = failure.as_str(),
                locked_out = locked,
                "network API authentication failed"
            );
            let mut response = error_response(&ProtoError::new(
                ErrorCode::Unauthorized,
                "a valid bearer token is required (Authorization: Bearer <token>)",
            ));
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            return close_after(response);
        }
    };

    // A WebSocket keeps this, to re-check before every command it runs.
    let mut request = request;
    request.extensions_mut().insert(credential);
    let started = Instant::now();
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    let status = response.status();
    let ms = started.elapsed().as_millis();
    if status.is_client_error() || status.is_server_error() {
        info!(peer = %peer.ip(), %method, %path, status = status.as_u16(), ms, "network API request");
    } else {
        debug!(peer = %peer.ip(), %method, %path, status = status.as_u16(), ms, "network API request");
    }
    response
}

/// `429` for a throttled request, with `Retry-After`.
pub(crate) fn throttled_response(throttled: Throttled) -> Response {
    let mut response = error_response(&throttled_error(throttled));
    if let Ok(v) = HeaderValue::from_str(&throttled.retry_after_secs().to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, v);
    }
    response
}

/// The protocol error for a throttled request (HTTP and WebSocket alike).
pub(crate) fn throttled_error(throttled: Throttled) -> ProtoError {
    let message = match throttled {
        Throttled::Rate(_) => "too many requests from this address",
        Throttled::LockedOut(_) => "too many failed authentications from this address",
        Throttled::Saturated(_) => "too many addresses are locked out; try again later",
    };
    ProtoError::new(ErrorCode::RateLimited, message)
        .with_detail(serde_json::json!({ "retry_after_s": throttled.retry_after_secs() }))
}

async fn not_found() -> Response {
    error_response(&ProtoError::new(ErrorCode::NotFound, "no such endpoint"))
}

/// Open a connection to the daemon and handshake as a remote client. The
/// grant comes from the transport, never from `client.kind`.
async fn connect(state: &AppState, name: &str) -> Result<Box<dyn Session>, ProtoError> {
    let mut session = state.backend.open();
    session
        .execute(
            Command::Handshake(Hello::new(ClientInfo::new(name, ClientKind::Remote))),
            never(),
        )
        .await?;
    Ok(session)
}

/// `POST /v1/transcribe` query parameters: the audio layout and the
/// [`SessionOptions`]. Every option goes to the daemon unchanged, so a
/// forbidden one (`inject=true`, `route=timer`) is answered `forbidden` by the
/// same check the socket uses.
#[derive(Debug, Default, Deserialize)]
struct TranscribeQuery {
    encoding: Option<String>,
    sample_rate_hz: Option<u32>,
    channels: Option<u16>,
    route: Option<String>,
    inject: Option<bool>,
    format_llm: Option<bool>,
    use_dictionary: Option<bool>,
    language: Option<String>,
    app: Option<String>,
    privacy: Option<bool>,
}

async fn transcribe(
    State(state): State<Arc<AppState>>,
    query: Result<Query<TranscribeQuery>, QueryRejection>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Ok(Query(query)) = query else {
        return error_response(&ProtoError::new(
            ErrorCode::InvalidParams,
            "unparseable query parameters",
        ));
    };
    let max = state.max_upload_bytes;
    let too_large = || {
        error_response(
            &ProtoError::new(
                ErrorCode::PayloadTooLarge,
                format!("the upload exceeds {max} bytes"),
            )
            .with_detail(serde_json::json!({ "limit_bytes": max })),
        )
    };
    // Refused from the header when it says so, before reading a byte.
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|len| len > max)
    {
        return too_large();
    }
    let limit = usize::try_from(max).unwrap_or(usize::MAX);
    let data = match tokio::time::timeout(BODY_TIMEOUT, axum::body::to_bytes(body, limit)).await {
        Err(_) => {
            return json_response(
                StatusCode::REQUEST_TIMEOUT,
                &ProtoError::new(
                    ErrorCode::Timeout,
                    format!(
                        "the upload did not arrive within {} s",
                        BODY_TIMEOUT.as_secs()
                    ),
                ),
            )
        }
        Ok(Err(e)) if is_length_limit(&e) => return too_large(),
        Ok(Err(_)) => {
            return error_response(&ProtoError::new(
                ErrorCode::MalformedRequest,
                "the upload body could not be read",
            ))
        }
        Ok(Ok(bytes)) => bytes,
    };

    let format = match audio_format(&query, &headers) {
        Ok(f) => f,
        Err(e) => return error_response(&e),
    };
    let route = match query.route.as_deref().map(parse_route).transpose() {
        Ok(r) => r,
        Err(e) => return error_response(&e),
    };
    let options = SessionOptions {
        route,
        inject: query.inject,
        format_llm: query.format_llm,
        use_dictionary: query.use_dictionary,
        language: query.language,
        app: query.app,
        privacy: query.privacy,
    };

    // A client that hangs up mid-request makes hyper drop this future, and
    // with it the session: the daemon then cancels the upload it owns.
    let mut session = match connect(&state, "http").await {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };
    let command = Command::TranscribeAudio {
        audio: AudioSource::Inline {
            format,
            data: data.to_vec(),
        },
        options: Some(options),
    };
    match session.execute(command, never()).await {
        Ok(result @ CommandResult::Transcript(_)) => json_response(StatusCode::OK, &result),
        Ok(other) => error_response(&ProtoError::new(
            ErrorCode::Internal,
            format!("unexpected result '{}'", other.name()),
        )),
        Err(e) => error_response(&e),
    }
}

fn is_length_limit(error: &axum::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = source {
        if e.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        source = e.source();
    }
    false
}

/// The upload's layout: `?encoding=` wins, then `Content-Type`, then WAV
/// (whose header describes itself; anything else fails to decode).
fn audio_format(query: &TranscribeQuery, headers: &HeaderMap) -> Result<AudioFormat, ProtoError> {
    let encoding = match &query.encoding {
        Some(name) => {
            serde_json::from_value::<AudioEncoding>(serde_json::Value::String(name.clone()))
                .map_err(|_| ProtoError::new(ErrorCode::InvalidParams, "invalid encoding"))?
        }
        None => {
            let content_type = headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|v| {
                    v.split(';')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_ascii_lowercase()
                });
            match content_type.as_deref() {
                None
                | Some(
                    "audio/wav"
                    | "audio/x-wav"
                    | "audio/wave"
                    | "audio/vnd.wave"
                    | "application/octet-stream",
                ) => AudioEncoding::Wav,
                Some(other) => {
                    return Err(ProtoError::new(
                        ErrorCode::AudioFormatUnsupported,
                        format!(
                            "unsupported Content-Type '{other}'; send audio/wav, or raw PCM \
                             with ?encoding=pcm_s16le|pcm_f32le&sample_rate_hz=&channels="
                        ),
                    ))
                }
            }
        }
    };
    Ok(AudioFormat {
        encoding,
        sample_rate_hz: query.sample_rate_hz,
        channels: query.channels,
    })
}

fn parse_route(name: &str) -> Result<Route, ProtoError> {
    serde_json::from_value::<Route>(serde_json::Value::String(name.to_string()))
        .map_err(|_| ProtoError::new(ErrorCode::InvalidParams, "invalid route"))
}

async fn status(State(state): State<Arc<AppState>>) -> Response {
    let mut session = match connect(&state, "http").await {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };
    match session.execute(Command::GetStatus, never()).await {
        Ok(result) => json_response(StatusCode::OK, &result),
        Err(e) => error_response(&e),
    }
}

async fn ws(
    State(state): State<Arc<AppState>>,
    axum::Extension(PeerAddr(peer)): axum::Extension<PeerAddr>,
    axum::Extension(credential): axum::Extension<Credential>,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    let upgrade = match upgrade {
        Ok(u) => u,
        Err(rejection) => return rejection.into_response(),
    };
    let Ok(permit) = state.ws_slots.clone().try_acquire_owned() else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &ProtoError::new(ErrorCode::Busy, "too many WebSocket sessions"),
        );
    };
    // The most any message on this connection may ever be; each one is then
    // held to the connection's current limit before it is parsed.
    let limit = state.ws_message_limit;
    upgrade
        .max_message_size(limit)
        .max_frame_size(limit)
        .on_upgrade(move |socket| async move {
            // Run in the tracked set, not in the task hyper spawned for the
            // upgrade, so shutdown can wait for the session and abort it.
            let tasks = state.ws_tasks.clone();
            tasks.spawn(async move {
                crate::ws::run(socket, state, peer, credential).await;
                drop(permit);
            });
        })
}
