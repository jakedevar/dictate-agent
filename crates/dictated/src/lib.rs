//! `dictated` — the dictation daemon.
//!
//! Hosts [`dictate_core`]'s engine behind two control surfaces that are the
//! same code path underneath:
//!
//! - a **unix-socket JSON-RPC** control plane speaking [`dictate_proto`] with
//!   NDJSON framing ([`server`]), and
//! - the **SIGUSR1/SIGUSR2 shim** that keeps `scripts/dictate-toggle` working
//!   ([`signals`]).
//!
//! It also owns the runtime paths ([`paths`]) that keep this daemon and the
//! Python reference daemon from standing on each other while both are
//! installed.
//!
//! # Testability is the reason this is a library
//!
//! `dictated` is a `lib` + `bin` rather than a bare binary so integration tests
//! can start a *real* daemon — real socket, real engine, real state machine —
//! against [`dictate_core::ports::mock`] doubles. Everything in
//! `tests/control_plane.rs` exercises the same [`Daemon::start`] the binary
//! calls; nothing about the concurrency behavior is re-implemented for tests.

pub mod paths;
pub mod server;
pub mod signals;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use dictate_core::config::Config;
use dictate_core::engine::DaemonIdentity;
use dictate_core::ports::{
    DesktopNotifier, GrammarFormatter, HostAudioSource, HostInjector, PlayerctlMedia, TextInjector,
    WhisperStt,
};
use dictate_core::session::ClientIdGen;
use dictate_core::{Engine, EngineHandle, EventBus, Pipeline, ResolvedOptions};
use dictate_history::HistoryStore;
use dictate_proto::Capabilities;
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::paths::{PidFile, RuntimePaths};
use crate::server::{local_capabilities, Server, ServerDeps};

/// A daemon that has bound its socket and is serving.
pub struct Daemon {
    socket: PathBuf,
    engine: EngineHandle,
    shutdown: Arc<Notify>,
    server: tokio::task::JoinHandle<()>,
    engine_task: tokio::task::JoinHandle<()>,
    /// Held for the daemon's lifetime; released on drop.
    _pid: Option<PidFile>,
}

impl Daemon {
    /// Assemble and start a daemon around an already-built pipeline.
    ///
    /// Taking the [`Pipeline`] as a parameter rather than building it here is
    /// what lets an integration test substitute mock ports while running
    /// everything else — engine, socket, framing, dispatch — for real.
    ///
    /// # Errors
    ///
    /// If the socket cannot be bound.
    pub async fn start(
        pipeline: Arc<Pipeline>,
        history: Arc<Mutex<HistoryStore>>,
        runtime: &RuntimePaths,
        capabilities: Capabilities,
        pid: Option<PidFile>,
    ) -> Result<Self> {
        let bus = EventBus::default();
        let (engine, handle) = Engine::new(pipeline, bus, DaemonIdentity::default());
        let engine_task = tokio::spawn(engine.run());

        let server = Server::bind(&runtime.socket).await?;
        let socket = server.path().to_path_buf();

        let deps = Arc::new(ServerDeps {
            engine: handle.clone(),
            history,
            ids: Arc::new(ClientIdGen::default()),
            capabilities,
        });

        let shutdown = Arc::new(Notify::new());
        let server_shutdown = shutdown.clone();
        let server = tokio::spawn(async move {
            server.serve(deps, async move { server_shutdown.notified().await }).await;
        });

        Ok(Self {
            socket,
            engine: handle,
            shutdown,
            server,
            engine_task,
            _pid: pid,
        })
    }

    /// The bound control socket.
    #[must_use]
    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    /// A handle to the engine, for the signal shim and for tests.
    #[must_use]
    pub fn engine(&self) -> &EngineHandle {
        &self.engine
    }

    /// Stop accepting, cancel any session in flight, and wait for the tasks.
    pub async fn shutdown(self) {
        info!("shutting down");
        self.shutdown.notify_waiters();
        // Cancels an in-flight session, which is what releases the microphone
        // — a daemon that exits while recording leaves the device open.
        self.engine.shutdown().await;
        let _ = self.server.await;
        let _ = self.engine_task.await;
    }
}

/// Build the production pipeline from configuration.
///
/// # Errors
///
/// If the audio device or the history database cannot be opened.
pub fn build_pipeline(config: &Config) -> Result<(Arc<Pipeline>, Arc<Mutex<HistoryStore>>, bool)> {
    let audio = Arc::new(HostAudioSource::new().context("opening the audio capture device")?);
    let stt = Arc::new(WhisperStt::new(&config.whisper));
    let formatter = Arc::new(GrammarFormatter::new(&config.grammar));
    let injector = Arc::new(HostInjector::new(&config.output));
    let injection_available = injector.is_available();
    let notifier = Arc::new(DesktopNotifier::new(&config.notifications));
    let media = Arc::new(PlayerctlMedia);

    // Default the history database to this daemon's own path rather than the
    // Python daemon's, unless the user has named one explicitly.
    let mut history_config = config.history.clone();
    if history_config.db_path.is_empty() {
        history_config.db_path = paths::default_history_db().to_string_lossy().into_owned();
    }
    let history = Arc::new(Mutex::new(
        HistoryStore::new(&history_config).context("opening the history database")?,
    ));

    let pipeline = Arc::new(Pipeline {
        audio,
        stt,
        formatter,
        injector,
        notifier,
        media,
        history: history.clone(),
        local: Arc::new(dictate_core::local_executor::LocalExecutor::new(&config.local)),
        timer: Arc::new(dictate_core::timer::TimerExecutor::new(&config.timer)),
        local_model: config.local.model.clone(),
    });

    Ok((pipeline, history, injection_available))
}

/// Run the daemon: pipeline, socket, signal shim, and shutdown.
///
/// # Errors
///
/// If the PID file is held by a live daemon, the socket cannot be bound, or a
/// device cannot be opened.
pub async fn run(config: Config) -> Result<()> {
    let runtime = RuntimePaths::from_env();
    runtime.ensure_dirs()?;

    let mut pid = PidFile::acquire(&runtime.pid)?;
    // Claim-if-free: keeps `dictate-toggle` working against this daemon
    // without ever stealing the reference daemon's signal channel.
    if !pid.claim_legacy(&runtime.legacy_pid) {
        warn!(
            "another daemon holds {}; scripts/dictate-toggle will drive that process, not this one",
            runtime.legacy_pid.display()
        );
    }

    let (pipeline, history, injection_available) = build_pipeline(&config)?;
    // Best-effort, matching today's startup: the LLM pass fails open, so a
    // missing Ollama must not stop the daemon from starting.
    {
        let (host, port) = dictate_fmt::grammar::parse_host_port(&config.grammar.host);
        tokio::spawn(async move {
            dictate_core::local_executor::ensure_ollama_running(&host, port, 10).await;
        });
    }

    let mut capabilities = local_capabilities(injection_available);
    capabilities.features.privacy_mode = history
        .lock()
        .map(|store| store.is_privacy_mode())
        .unwrap_or(false);
    let signal_options = ResolvedOptions {
        inject: capabilities.features.text_injection,
        forced_route: None,
        allowed_routes: capabilities.routes.clone(),
        privacy: false,
        app: None,
    };

    let daemon = Daemon::start(pipeline, history, &runtime, capabilities, Some(pid)).await?;
    info!(
        socket = %daemon.socket().display(),
        pid = std::process::id(),
        "dictated ready"
    );

    // The signal shim owns the daemon's lifetime: it returns on SIGINT/SIGTERM.
    signals::listen(daemon.engine().clone(), signal_options).await?;
    daemon.shutdown().await;
    Ok(())
}
