//! The listener: bind per the plan, accept within the connection cap, run each
//! connection's TLS handshake (if any) and HTTP/1.1 in its own task.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{TcpListener, TcpStream, UnixListener};
use tokio::sync::{watch, Notify, Semaphore};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;
use tracing::{debug, info, warn};

use crate::backend::Backend;
use crate::config::{ApiConfig, BindPlan, BindRefusal, Transport};
use crate::guard::HostPolicy;
use crate::http::{router, AppState, PeerAddr};
use crate::limit::{LogBudget, Throttle};
use crate::token::{TokenError, TokenStore};
use crate::ws::{WsTasks, CLOSE_TIMEOUT};

/// The request head must arrive within this (slowloris).
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// The TLS handshake must finish within this.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Why the API did not start. Nothing listens after any of these. The
/// daemon's startup logs it and keeps dictating (`dictated`'s
/// `serve_network_api`); an unsafe *configuration* is refused earlier, as a
/// config validation error.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// A private Unix listener could not be secured or bound.
    #[error("network API Unix socket refused: {0}")]
    Unix(#[from] std::io::Error),
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
    addr: Option<SocketAddr>,
    unix: Option<crate::unix::PrivateSocket>,
    loopback: bool,
    tls_fingerprint: Option<String>,
    stop: Arc<Notify>,
    closing: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    ws_tasks: Arc<WsTasks>,
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
        let unix = plan
            .socket
            .as_deref()
            .map(crate::unix::PrivateSocket::bind)
            .transpose()?;
        let listener = if unix.is_none() {
            Some(
                TcpListener::bind(plan.addr)
                    .await
                    .map_err(|source| StartError::Bind {
                        addr: plan.addr,
                        source,
                    })?,
            )
        } else {
            None
        };
        let addr = listener
            .as_ref()
            .map(TcpListener::local_addr)
            .transpose()
            .map_err(|source| StartError::Bind {
                addr: plan.addr,
                source,
            })?;
        // The Host policy uses the port actually bound (port 0 in tests).
        let bound = BindPlan {
            addr: addr.unwrap_or(plan.addr),
            ..plan
        };

        let (closing, closing_rx) = watch::channel(false);
        let ws_tasks = Arc::new(WsTasks::new());
        let ws_message_limit = backend.max_message_bytes();
        let state = Arc::new(AppState {
            backend,
            tokens,
            throttle: Arc::new(Throttle::new(config.requests_per_minute, config.burst)),
            hosts: HostPolicy::new(&bound, &config.allowed_hosts),
            origins: config.allowed_origins.clone(),
            max_upload_bytes: config.max_upload_bytes,
            ws_message_limit,
            ws_slots: Arc::new(Semaphore::new(config.max_ws_sessions as usize)),
            ws_tasks: ws_tasks.clone(),
            log_budget: LogBudget::default(),
            closing: closing_rx,
        });
        let app = router(state);

        let scheme = if tls.is_some() { "https" } else { "http" };
        if let Some(path) = &bound.socket {
            info!(
                "network API listening on private Unix socket {} (no TCP)",
                path.display()
            );
        } else if bound.loopback {
            info!(
                "network API listening on {scheme}://{} (loopback only)",
                bound.addr
            );
        } else {
            warn!(
                "network API listening BEYOND this host on {scheme}://{} ({})",
                bound.addr,
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
            unix.as_ref().map(|u| u.listener()).transpose()?,
            app,
            tls,
            Arc::new(Semaphore::new(config.max_connections as usize)),
            stop.clone(),
        ));
        Ok(Some(Self {
            addr,
            unix,
            loopback: bound.loopback,
            tls_fingerprint,
            stop,
            closing,
            task,
            ws_tasks,
        }))
    }

    /// The bound address.
    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.addr
    }

    /// The HTTP Unix socket, when TCP is absent.
    pub fn socket_path(&self) -> Option<&Path> {
        self.unix.as_ref().map(|u| u.path())
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
    /// it owned). Returns once every session has ended: those that do not
    /// close within the close handshake's bound are aborted.
    pub async fn shutdown(self) {
        let _ = self.closing.send(true);
        // `notify_one` stores a permit, so this is not lost if the loop is
        // between polls.
        self.stop.notify_one();
        let _ = self.task.await;
        // A session closes in at most two CLOSE_TIMEOUTs (the close frame,
        // then the socket); a little more, then abort.
        self.ws_tasks
            .shutdown(CLOSE_TIMEOUT * 2 + Duration::from_millis(500))
            .await;
        info!("network API stopped");
    }
}

async fn accept_loop(
    listener: Option<TcpListener>,
    unix: Option<UnixListener>,
    app: axum::Router,
    tls: Option<TlsAcceptor>,
    slots: Arc<Semaphore>,
    stop: Arc<Notify>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = stop.notified() => break,
            accepted = async { listener.as_ref().expect("TCP branch").accept().await }, if listener.is_some() => {
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
            accepted = async { unix.as_ref().expect("Unix branch").accept().await }, if unix.is_some() => {
                match accepted {
                    Ok((stream, _)) => {
                        let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                        let app = app.clone();
                        connections.spawn(async move {
                            // One shared throttle bucket for local UDS callers. Their
                            // grant remains the network grant, never local_trusted.
                            serve_plain(stream, "127.0.0.1:0".parse().expect("literal"), app).await;
                            drop(permit);
                        });
                    }
                    Err(e) => {
                        warn!("network API Unix accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
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
    match tls {
        None => serve_plain(tcp, peer, app).await,
        Some(acceptor) => {
            match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
                Ok(Ok(stream)) => serve_plain(stream, peer, app).await,
                Ok(Err(e)) => debug!(peer = %peer.ip(), "TLS handshake failed: {e}"),
                Err(_) => debug!(peer = %peer.ip(), "TLS handshake timed out"),
            }
        }
    }
}

async fn serve_plain<T>(stream: T, peer: SocketAddr, app: axum::Router)
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
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
    let result = http
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await;
    if let Err(e) = result {
        debug!(peer = %peer.ip(), "network API connection ended: {e}");
    }
}
