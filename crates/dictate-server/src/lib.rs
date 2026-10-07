//! `dictate-server` — the network API (S33): `dictate-proto` over HTTP and
//! WebSocket, for a thin client (a phone) and, later, sync.
//!
//! # Security model in one screen
//!
//! The full design is
//! `thoughts/shared/plans/2026-10-07-s33-network-api-security-design.md`.
//!
//! - **Off by default; loopback when on.** `[api] enabled = false` opens no
//!   port. Enabled, it binds `127.0.0.1:7313`. A non-loopback bind needs
//!   `allow_lan` *and* TLS or an explicit encrypted-tunnel acknowledgment;
//!   every-interface binds are refused outright ([`ApiConfig::bind_plan`]).
//! - **Always authenticated.** Every request — loopback included, since
//!   loopback TCP has no peer-UID check — needs `Authorization: Bearer` with
//!   the 256-bit token from a 0600 file the daemon never creates on its own
//!   ([`token`]). No cookies, no query-string tokens.
//! - **Not reachable from a browser.** `Host` must name the bound address
//!   (DNS rebinding), any `Origin` is refused (CSRF, cross-site WebSocket
//!   hijacking), and no CORS header is ever sent ([`guard`]).
//! - **Transcription only.** The grant is `remote_transcription_only`,
//!   clamped by [`network_ceiling`]: no injection, microphone, window context,
//!   config, history, dictionary or diagnostics; route `type` only.
//!   `raw_text` is an opt-in.
//! - **No second dispatcher.** Commands run through [`Backend`], which
//!   `dictated` implements over the unix socket's own dispatch.
//! - **Bounded.** Connection and session caps, header/body/handshake/idle
//!   timeouts, a streaming body cap, per-IP throttling and an
//!   authentication-failure lockout ([`limit`]).

pub mod backend;
pub mod config;
pub mod guard;
mod http;
pub mod limit;
mod serve;
pub mod tls;
pub mod token;
mod ws;

pub use backend::{never, Backend, BoxFuture, Hangup, Session};
pub use config::{
    default_token_path, network_ceiling, ApiConfig, BindPlan, BindRefusal, Transport, DEFAULT_PORT,
};
pub use serve::{ApiServer, StartError};
pub use token::{TokenError, TokenStore, TOKEN_PREFIX};
