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

pub mod config_rpc;
pub mod doctor;
pub mod paths;
pub mod server;
pub mod signals;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use dictate_core::config::{Config, ConfigReport};
use dictate_core::engine::DaemonIdentity;
use dictate_core::ports::{
    AudioSource, DesktopNotifier, DisabledAudioSource, GrammarFormatter, HostAudioSource,
    HostEarcons, HostInjector, PlayerctlMedia, StatusNotifier, TextInjector, WhisperStt,
};
use dictate_core::session::ClientIdGen;
use dictate_core::{Engine, EngineHandle, EventBus, Pipeline, ResolvedOptions};
use dictate_history::HistoryStore;
use dictate_proto::Capabilities;
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::paths::{PidFile, RuntimePaths};
use crate::server::{local_capabilities, DiagnosticsProvider, Server, ServerDeps};

/// Optional collaborators of a daemon, so `Daemon::start`'s signature stays
/// stable as capabilities are added.
#[derive(Default)]
pub struct DaemonExtras {
    /// Answers `diagnose`. Without one the command is `capability_unavailable`.
    pub diagnostics: Option<Arc<dyn DiagnosticsProvider>>,
    /// Answers `get_config` / `set_config`. Without one the `config_read` and
    /// `config_write` capabilities are withdrawn.
    pub config: Option<Arc<config_rpc::ConfigService>>,
}

/// A daemon that has bound its socket and is serving.
pub struct Daemon {
    socket: PathBuf,
    engine: EngineHandle,
    shutdown: Arc<Notify>,
    server: tokio::task::JoinHandle<()>,
    engine_task: tokio::task::JoinHandle<()>,
    dictionary_flush: Option<tokio::task::JoinHandle<()>>,
    dictionary: Option<Arc<dictate_dict::Dictionary>>,
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
        Self::start_with(
            pipeline,
            history,
            runtime,
            capabilities,
            pid,
            DaemonExtras::default(),
        )
        .await
    }

    /// As [`Daemon::start`], with optional collaborators.
    ///
    /// # Errors
    ///
    /// If the socket cannot be bound.
    pub async fn start_with(
        pipeline: Arc<Pipeline>,
        history: Arc<Mutex<HistoryStore>>,
        runtime: &RuntimePaths,
        mut capabilities: Capabilities,
        pid: Option<PidFile>,
        extras: DaemonExtras,
    ) -> Result<Self> {
        let dictionary = pipeline.dictionary.clone();
        if dictionary.is_none() {
            capabilities.features.dictionary_read = false;
            capabilities.features.dictionary_write = false;
        }
        if extras.config.is_none() {
            capabilities.features.config_read = false;
            capabilities.features.config_write = false;
        }
        let bus = EventBus::default();
        let (engine, handle) = Engine::new(pipeline, bus, DaemonIdentity::default());
        let engine_task = tokio::spawn(engine.run());

        let server = Server::bind(&runtime.socket).await?;
        let socket = server.path().to_path_buf();

        let deps = Arc::new(ServerDeps {
            engine: handle.clone(),
            history,
            dictionary: dictionary.clone(),
            ids: Arc::new(ClientIdGen::default()),
            capabilities,
            diagnostics: extras.diagnostics,
            config: extras.config,
        });

        let shutdown = Arc::new(Notify::new());
        let server_shutdown = shutdown.clone();
        let server = tokio::spawn(async move {
            server
                .serve(deps, async move { server_shutdown.notified().await })
                .await;
        });

        // Hit batches run off the transcription hot path. Shutdown flushes the
        // last batch too; failures remain queued and are visible in diagnostics.
        let dictionary_flush = dictionary.clone().map(|dictionary| {
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
                loop {
                    tick.tick().await;
                    let dictionary = dictionary.clone();
                    match tokio::task::spawn_blocking(move || dictionary.flush_hits()).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => warn!("dictionary hit flush failed: {e}"),
                        Err(e) => warn!("dictionary hit worker failed: {e}"),
                    }
                }
            })
        });

        Ok(Self {
            dictionary_flush,
            dictionary,
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
        if let Some(task) = self.dictionary_flush {
            task.abort();
            let _ = task.await;
        }
        if let Some(dictionary) = self.dictionary {
            match tokio::task::spawn_blocking(move || dictionary.flush_hits()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warn!("final dictionary hit flush failed: {e}"),
                Err(e) => warn!("final dictionary hit worker failed: {e}"),
            }
        }
    }
}

/// Build the production pipeline from configuration.
///
/// # Errors
///
/// If the audio device or the history database cannot be opened.
pub fn build_pipeline(config: &Config) -> Result<(Arc<Pipeline>, Arc<Mutex<HistoryStore>>, bool)> {
    // Audio-less mode wires in a source that *cannot* open a device, so the
    // guarantee that no microphone is touched does not depend on every code
    // path remembering to check a flag.
    let audio: Arc<dyn AudioSource> = if config.audio.capture {
        Arc::new(
            HostAudioSource::new(config.audio.clone())
                .context("opening the audio capture device")?,
        )
    } else {
        info!("audio capture disabled ([audio] capture = false): no input device will be opened");
        Arc::new(DisabledAudioSource::new(&config.audio))
    };
    let stt = Arc::new(WhisperStt::new(&config.whisper));
    let vad = Arc::new(
        dictate_vad::SileroVad::new(config.vad.clone()).context("loading VAD configuration")?,
    );
    let notifier = Arc::new(DesktopNotifier::new(&config.notifications));
    // The formatter announces its own failure (one desktop notification) through
    // the same notifier the pipeline uses.
    let formatter = Arc::new(
        GrammarFormatter::new(&config.grammar)
            .with_notifier(notifier.clone() as Arc<dyn StatusNotifier>),
    );
    let injector = Arc::new(HostInjector::new(&config.output));
    let injection_available = injector.is_available();
    let media = Arc::new(PlayerctlMedia);
    let earcons = Arc::new(HostEarcons::new(config.audio.earcons.clone()));

    // Default the history database to this daemon's own path rather than the
    // Python daemon's, unless the user has named one explicitly.
    let mut history_config = config.history.clone();
    if history_config.db_path.is_empty() {
        history_config.db_path = paths::default_history_db().to_string_lossy().into_owned();
    }
    let history = Arc::new(Mutex::new(
        HistoryStore::new(&history_config).context("opening the history database")?,
    ));

    let dictionary = Arc::new(
        dictate_dict::Dictionary::open(
            config.dictionary.clone(),
            config.whisper.initial_prompt.clone(),
        )
        .context("opening the dictionary database")?,
    );
    let pipeline = Arc::new(Pipeline {
        context: Arc::new(dictate_core::ContextEngine::from_config(
            config.context.clone(),
        )),
        dictionary: Some(dictionary),
        audio,
        stt,
        vad,
        formatter,
        injector,
        notifier,
        media,
        earcons,
        history: history.clone(),
        local: Arc::new(dictate_core::local_executor::LocalExecutor::new(
            &config.local,
        )),
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
    run_with_report(config, ConfigReport::default()).await
}

/// As [`run`], keeping what loading the config file found so `dictate doctor`
/// can show it.
///
/// # Errors
///
/// As [`run`].
pub async fn run_with_report(config: Config, report: ConfigReport) -> Result<()> {
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
    // missing Ollama must not stop the daemon from starting. But failing open
    // must not mean failing *silently*: once Ollama is (or is not) up, ask it
    // whether the configured model exists, and say so — in the log, in
    // `get_status`, and with one desktop notification — before the first
    // dictation rather than never.
    {
        let (host, port) = dictate_fmt::grammar::parse_host_port(&config.grammar.host);
        let formatter = pipeline.formatter.clone();
        tokio::spawn(async move {
            dictate_core::local_executor::ensure_ollama_running(&host, port, 10).await;
            formatter.probe().await;
        });
    }

    let mut capabilities = local_capabilities(injection_available);
    capabilities.limits = config.upload.limits();
    capabilities.features.privacy_mode = history
        .lock()
        .map(|store| store.is_privacy_mode())
        .unwrap_or(false);
    let signal_options = ResolvedOptions {
        inject: capabilities.features.text_injection,
        forced_route: None,
        allowed_routes: capabilities.routes.clone(),
        privacy: false,
        use_dictionary: true,
        app: None,
        ..Default::default()
    };

    let config_service = Arc::new(
        config_rpc::ConfigService::new(report.path.clone(), &config).with_history(history.clone()),
    );
    let doctor = Arc::new(doctor::Doctor::new(
        config.clone(),
        report,
        runtime.clone(),
        pipeline.clone(),
    ));
    let daemon = Daemon::start_with(
        pipeline,
        history,
        &runtime,
        capabilities,
        Some(pid),
        DaemonExtras {
            diagnostics: Some(doctor),
            config: Some(config_service),
        },
    )
    .await?;
    info!(
        socket = %daemon.socket().display(),
        pid = std::process::id(),
        "dictated ready"
    );

    // Evdev is optional and deliberately degrades to this unchanged signal
    // path when input-device permissions have not been granted.
    let hotkey = dictate_hotkey::HotkeyService::start(
        &config.hotkey,
        daemon.engine().clone(),
        signal_options.clone(),
    );

    // The signal shim owns the daemon's lifetime: it returns on SIGINT/SIGTERM.
    signals::listen(daemon.engine().clone(), signal_options).await?;
    hotkey.shutdown();
    daemon.shutdown().await;
    Ok(())
}
