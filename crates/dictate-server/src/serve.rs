//! The listener: bind per the plan, accept within the connection cap, run each
//! connection's TLS handshake (if any) and HTTP/1.1 in its own task.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Notify, Semaphore};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;
use tracing::{debug, info, warn};

use crate::backend::Backend;
use crate::config::{ApiConfig, BindPlan, BindRefusal, Transport};
use crate::guard::HostPolicy;
use crate::http::{router, AppState, PeerAddr};
use crate::limit::Throttle;
use crate::token::{TokenError, TokenStore};

/// The request head must arrive within this (slowloris).
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// The TLS handshake must finish within this.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Why the API did not start. Each is fatal to the daemon's startup: an
/// operator who asked for a listener is told why there is none.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The configuration asks for more exposure than it opted into.
    #[error("network API refused: {0}")]
    Refused(#[from] BindRefusal),
    /// No usable token.
    #[error("network API refused: {0}")]
    Token(#[from] TokenError),
    /// TLS was configured but cannot be used.
    #[error("network API refused: TLS: {0}")]
    Tls(String),
    /// The address could not be bound.
    #[error("network API could not bind {addr}: {source}")]
    Bind {
        /// The address.
        addr: SocketAddr,
        /// Why.
        source: std::io::Error,
    },
}

/// A running network API.
pub struct ApiServer {
    addr: SocketAddr,
    loopback: bool,
    tls_fingerprint: Option<String>,
    stop: Arc<Notify>,
    closing: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ApiServer {
    /// Start the API described by `config`, serving `backend`. `Ok(None)`
    /// when the API is disabled.
    ///
    /// # Errors
    ///
    /// A [`StartError`] for every refusal (design §4): an unsafe bind, a
    /// missing or insecure token, unusable TLS, or a failed bind.
    pub async fn start(
        config: &ApiConfig,
        backend: Arc<dyn Backend>,
    ) -> Result<Option<Self>, StartError> {
        let Some(plan) = config.bind_plan()? else {
            return Ok(None);
        };
        let tokens = Arc::new(TokenStore::open(&config.token_path())?);
        let (tls, tls_fingerprint) = match &plan.transport {
            Transport::Plain => (None, None),
            Transport::Tls { cert, key } => {
                let (acceptor, fp) = crate::tls::acceptor(cert, key).map_err(StartError::Tls)?;
                (Some(acceptor), Some(fp))
            }
        };
        let listener = TcpListener::bind(plan.addr)
            .await
            .map_err(|source| StartError::Bind {
                addr: plan.addr,
                source,
            })?;
        let addr = listener.local_addr().map_err(|source| StartError::Bind {
            addr: plan.addr,
            source,
        })?;
        // The Host policy uses the port actually bound (port 0 in tests).
        let bound = BindPlan { addr, ..plan };

        let (closing, closing_rx) = watch::channel(false);
        let state = Arc::new(AppState {
            backend,
            tokens,
            throttle: Arc::new(Throttle::new(config.requests_per_minute, config.burst)),
            hosts: HostPolicy::new(&bound, &config.allowed_hosts),
            origins: config.allowed_origins.clone(),
            max_upload_bytes: config.max_upload_bytes,
            limits: config.limits(),
            ws_slots: Arc::new(Semaphore::new(config.max_ws_sessions as usize)),
            closing: closing_rx,
        });
        let app = router(state);

        let scheme = if tls.is_some() { "https" } else { "http" };
        if bound.loopback {
            info!("network API listening on {scheme}://{addr} (loopback only)");
        } else {
            warn!(
                "network API listening BEYOND this host on {scheme}://{addr} ({})",
                if tls.is_some() {
                    "TLS"
                } else {
                    "plaintext: allow_plaintext_lan asserts this is an encrypted tunnel"
                }
            );
        }

        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(accept_loop(
            listener,
            app,
            tls,
            Arc::new(Semaphore::new(config.max_connections as usize)),
            stop.clone(),
        ));
        Ok(Some(Self {
            addr,
            loopback: bound.loopback,
            tls_fingerprint,
            stop,
            closing,
            task,
        }))
    }

    /// The bound address.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Whether the listener is loopback-only.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        self.loopback
    }

    /// The served certificate's SHA-256 fingerprint, when TLS is on.
    #[must_use]
    pub fn tls_fingerprint(&self) -> Option<&str> {
        self.tls_fingerprint.as_deref()
    }

    /// Stop accepting, drop every HTTP connection, and close every WebSocket
    /// session (each one's daemon connection is dropped, which cancels what
    /// it owned).
    pub async fn shutdown(self) {
        let _ = self.closing.send(true);
        // `notify_one` stores a permit, so this is not lost if the loop is
        // between polls.
        self.stop.notify_one();
        let _ = self.task.await;
        info!("network API stopped");
    }
}

async fn accept_loop(
    listener: TcpListener,
    app: axum::Router,
    tls: Option<TlsAcceptor>,
    slots: Arc<Semaphore>,
    stop: Arc<Notify>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = stop.notified() => break,
            accepted = listener.accept() => {
                let (tcp, peer) = match accepted {
                    Ok(a) => a,
                    Err(e) => {
                        // EMFILE and friends: back off instead of spinning.
                        warn!("network API accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    debug!(peer = %peer.ip(), "network API connection limit reached; closing");
                    drop(tcp);
                    continue;
                };
                let app = app.clone();
                let tls = tls.clone();
                connections.spawn(async move {
                    serve_connection(tcp, peer, app, tls).await;
                    drop(permit);
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
}

async fn serve_connection(
    tcp: TcpStream,
    peer: SocketAddr,
    app: axum::Router,
    tls: Option<TlsAcceptor>,
) {
    let _ = tcp.set_nodelay(true);
    let service = hyper::service::service_fn(move |mut request: hyper::Request<Incoming>| {
        request.extensions_mut().insert(PeerAddr(peer));
        app.clone().oneshot(request)
    });
    let mut http = hyper::server::conn::http1::Builder::new();
    // The header timer runs whenever hyper waits for a request head —
    // including the idle gap between keep-alive requests — so an idle
    // connection holds its slot for at most HEADER_TIMEOUT. Keep-alive stays
    // on because, with the read side open, hyper notices a client that hangs
    // up mid-request and drops the request (and with it the daemon
    // connection, which cancels the upload). Every request still passes the
    // whole admission gate; nothing is cached per connection.
    http.timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .keep_alive(true)
        .half_close(false);
    let result = match tls {
        None => {
            http.serve_connection(TokioIo::new(tcp), service)
                .with_upgrades()
                .await
        }
        Some(acceptor) => {
            match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
                Ok(Ok(stream)) => {
                    http.serve_connection(TokioIo::new(stream), service)
                        .with_upgrades()
                        .await
                }
                Ok(Err(e)) => {
                    debug!(peer = %peer.ip(), "TLS handshake failed: {e}");
                    return;
                }
                Err(_) => {
                    debug!(peer = %peer.ip(), "TLS handshake timed out");
                    return;
                }
            }
        }
    };
    if let Err(e) = result {
        debug!(peer = %peer.ip(), "network API connection ended: {e}");
    }
}
