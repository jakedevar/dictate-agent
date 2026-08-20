//! The engine: one task, one mailbox, one session slot.
//!
//! # Why an actor rather than a lock
//!
//! Every hard case in this slice is a race between two commands: two
//! `StartDictation`s arriving together, a `Cancel` landing while the pipeline
//! is injecting, a client vanishing mid-session, a SIGUSR1 arriving while the
//! CLI is already talking to the socket. A shared `Mutex<DaemonState>` makes
//! each of those *individually* safe and collectively unanalyzable, because
//! the interesting question is never "is this field torn" but "which of these
//! two commands happened first".
//!
//! So command handling is serialized by construction: all commands — from
//! every connection and from the signal handler — go down one
//! [`mpsc`](tokio::sync::mpsc) channel into one task. Ordering is the channel's
//! ordering, there is no lock to forget, and "what happens if these race" has
//! a single answer: *one of them is simply second, and sees the state the
//! first one left*.
//!
//! The engine loop must therefore never block on the pipeline. It doesn't: a
//! session runs in its own task and reports back through the same mailbox, so
//! a `Cancel` is processed while transcription is still in flight.
//!
//! # The signal shim is not a second implementation
//!
//! SIGUSR1 and SIGUSR2 do not have their own recording logic. They construct
//! [`Actor::Signal`] and send [`EngineRequest::Toggle`] / `Cancel` down the
//! same channel the protocol commands use, and [`Engine::handle_toggle`]
//! dispatches to the very functions `start_dictation` and `stop` call. There is
//! no path by which the signal and socket routes can drift apart, because
//! there is only one path.

use std::sync::Arc;

use dictate_proto::{
    Capabilities, Command, DaemonInfo, DictationMode, ErrorCode, Event, ModelStatus, ProtoError,
    Route, SessionId, SessionOptions, SessionSummary, State, Status,
};
use tokio::sync::{mpsc, oneshot, Notify};
use tracing::{debug, info, warn};

use crate::cancel::{CancelToken, CancelVerdict};
use crate::event_bus::EventBus;
use crate::pipeline::{Pipeline, PipelineOutcome, ResolvedOptions};
use crate::session::{
    new_session_id, now_ms, Actor, ClientId, DisconnectAction, SessionHandle, SessionOwner,
};

/// Depth of the engine mailbox. Commands are handled in microseconds unless a
/// device is being opened, so this only ever buffers a burst.
const MAILBOX_DEPTH: usize = 64;

/// What a toggle resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToggleOutcome {
    /// A new session was started.
    Started(SessionId),
    /// The running session was asked to stop.
    Stopped(SessionId),
}

/// A message to the engine task.
pub enum EngineRequest {
    /// Begin a session.
    Start {
        /// Who asked.
        actor: Actor,
        /// Requested mode.
        mode: DictationMode,
        /// Options, already resolved against the caller's capabilities.
        options: ResolvedOptions,
        /// Where to send the answer.
        reply: oneshot::Sender<Result<SessionId, ProtoError>>,
    },
    /// End the recording phase of the active session.
    Stop {
        /// Who asked.
        actor: Actor,
        /// Where to send the answer.
        reply: oneshot::Sender<Result<SessionId, ProtoError>>,
    },
    /// Abandon the active session.
    Cancel {
        /// Who asked.
        actor: Actor,
        /// Where to send the answer.
        reply: oneshot::Sender<Result<SessionId, ProtoError>>,
    },
    /// Start if idle, stop if recording. The signal shim's entry point.
    Toggle {
        /// Who asked.
        actor: Actor,
        /// Options for the start branch.
        options: ResolvedOptions,
        /// Where to send the answer.
        reply: oneshot::Sender<Result<ToggleOutcome, ProtoError>>,
    },
    /// Report daemon and session status for a connection.
    Status {
        /// The asking connection's capabilities, echoed back in the answer.
        capabilities: Box<Capabilities>,
        /// Where to send the answer.
        reply: oneshot::Sender<Box<Status>>,
    },
    /// A control connection went away.
    Disconnected {
        /// Which connection.
        client: ClientId,
        /// Whether it was trusted with this host's microphone, which decides
        /// whether its session is orphaned or cancelled.
        host_capture: bool,
    },
    /// A session task reached a terminal state. Sent by the task itself.
    SessionFinished {
        /// Which session.
        session_id: SessionId,
        /// What it produced.
        outcome: Box<PipelineOutcome>,
    },
    /// Stop the engine, cancelling any session in flight.
    Shutdown {
        /// Signalled once shutdown is complete.
        reply: oneshot::Sender<()>,
    },
}

/// A cheap, clonable handle to the engine task.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<EngineRequest>,
    bus: EventBus,
}

impl EngineHandle {
    /// Subscribe to the event stream.
    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.bus.subscribe()
    }

    /// The event bus, for components that publish rather than subscribe.
    #[must_use]
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> EngineRequest,
    ) -> Result<T, ProtoError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(make(tx))
            .await
            .map_err(|_| ProtoError::new(ErrorCode::Internal, "engine is not running"))?;
        rx.await
            .map_err(|_| ProtoError::new(ErrorCode::Internal, "engine dropped the request"))
    }

    /// Start a dictation session.
    ///
    /// # Errors
    ///
    /// `busy` if a session is already running, `audio_device_error` if the
    /// microphone could not be opened.
    pub async fn start(
        &self,
        actor: Actor,
        mode: DictationMode,
        options: ResolvedOptions,
    ) -> Result<SessionId, ProtoError> {
        self.ask(|reply| EngineRequest::Start {
            actor,
            mode,
            options,
            reply,
        })
        .await?
    }

    /// Stop the active session's recording phase.
    ///
    /// # Errors
    ///
    /// `no_active_session`, `forbidden` if the caller does not own it, or
    /// `invalid_state` if it has already left `recording`.
    pub async fn stop(&self, actor: Actor) -> Result<SessionId, ProtoError> {
        self.ask(|reply| EngineRequest::Stop { actor, reply }).await?
    }

    /// Cancel the active session.
    ///
    /// # Errors
    ///
    /// `no_active_session`, `forbidden` if the caller does not own it, or
    /// `conflict` if injection has already been committed.
    pub async fn cancel(&self, actor: Actor) -> Result<SessionId, ProtoError> {
        self.ask(|reply| EngineRequest::Cancel { actor, reply }).await?
    }

    /// Start-if-idle, stop-if-recording.
    ///
    /// # Errors
    ///
    /// Whatever the resolved branch would return, or `busy` mid-pipeline.
    pub async fn toggle(
        &self,
        actor: Actor,
        options: ResolvedOptions,
    ) -> Result<ToggleOutcome, ProtoError> {
        self.ask(|reply| EngineRequest::Toggle {
            actor,
            options,
            reply,
        })
        .await?
    }

    /// Current daemon and session status.
    ///
    /// # Errors
    ///
    /// `internal` if the engine is gone.
    pub async fn status(&self, capabilities: Capabilities) -> Result<Box<Status>, ProtoError> {
        self.ask(|reply| EngineRequest::Status {
            capabilities: Box::new(capabilities),
            reply,
        })
        .await
    }

    /// Tell the engine a connection went away.
    pub async fn disconnected(&self, client: ClientId, host_capture: bool) {
        let _ = self
            .tx
            .send(EngineRequest::Disconnected {
                client,
                host_capture,
            })
            .await;
    }

    /// Stop the engine and wait for it to wind down.
    pub async fn shutdown(&self) {
        let _ = self.ask(|reply| EngineRequest::Shutdown { reply }).await;
    }
}

/// The session currently occupying the engine's one slot.
struct ActiveSession {
    handle: SessionHandle,
    owner: SessionOwner,
    stop: Arc<Notify>,
    /// Whether a `Stop` has already been issued, so a second one is
    /// `invalid_state` rather than a silent no-op.
    stop_issued: bool,
}

/// Daemon identity reported by `get_status`.
#[derive(Debug, Clone)]
pub struct DaemonIdentity {
    /// Process name, e.g. `dictated`.
    pub name: String,
    /// Crate version.
    pub version: String,
}

impl Default for DaemonIdentity {
    fn default() -> Self {
        Self {
            name: "dictated".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

/// The command-serializing state owner.
pub struct Engine {
    pipeline: Arc<Pipeline>,
    bus: EventBus,
    active: Option<ActiveSession>,
    identity: DaemonIdentity,
    started_ms: u64,
    tx: mpsc::Sender<EngineRequest>,
    rx: mpsc::Receiver<EngineRequest>,
}

impl Engine {
    /// Build an engine and its handle. Call [`Engine::run`] to drive it.
    #[must_use]
    pub fn new(pipeline: Arc<Pipeline>, bus: EventBus, identity: DaemonIdentity) -> (Self, EngineHandle) {
        let (tx, rx) = mpsc::channel(MAILBOX_DEPTH);
        let handle = EngineHandle {
            tx: tx.clone(),
            bus: bus.clone(),
        };
        let engine = Self {
            pipeline,
            bus,
            active: None,
            identity,
            started_ms: now_ms(),
            tx,
            rx,
        };
        (engine, handle)
    }

    /// Run until [`EngineRequest::Shutdown`] or every handle is dropped.
    pub async fn run(mut self) {
        info!("engine running");
        while let Some(req) = self.rx.recv().await {
            match req {
                EngineRequest::Start {
                    actor,
                    mode,
                    options,
                    reply,
                } => {
                    let r = self.handle_start(&actor, mode, options).await;
                    let _ = reply.send(r);
                }
                EngineRequest::Stop { actor, reply } => {
                    let _ = reply.send(self.handle_stop(&actor));
                }
                EngineRequest::Cancel { actor, reply } => {
                    let _ = reply.send(self.handle_cancel(&actor));
                }
                EngineRequest::Toggle {
                    actor,
                    options,
                    reply,
                } => {
                    let r = self.handle_toggle(&actor, options).await;
                    let _ = reply.send(r);
                }
                EngineRequest::Status {
                    capabilities,
                    reply,
                } => {
                    let _ = reply.send(Box::new(self.status(*capabilities)));
                }
                EngineRequest::Disconnected {
                    client,
                    host_capture,
                } => self.handle_disconnected(client, host_capture),
                EngineRequest::SessionFinished { session_id, .. } => {
                    self.handle_session_finished(&session_id);
                }
                EngineRequest::Shutdown { reply } => {
                    self.handle_shutdown();
                    let _ = reply.send(());
                    break;
                }
            }
        }
        info!("engine stopped");
    }

    /// Whether a session is occupying the slot and has not finished.
    fn has_live_session(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|s| !s.handle.is_finished())
    }

    async fn handle_start(
        &mut self,
        actor: &Actor,
        mode: DictationMode,
        options: ResolvedOptions,
    ) -> Result<SessionId, ProtoError> {
        // `max_concurrent_sessions` is 1. Two racing starts are not a data
        // race — they are simply ordered by the mailbox, and the second one
        // sees the first one's session and is told to back off.
        if self.has_live_session() {
            return Err(ProtoError::new(
                ErrorCode::Busy,
                "a dictation session is already running",
            )
            .with_retry_after_ms(500));
        }

        // Opening the device is awaited here, inside the serialized loop, so
        // that `session_started` is never reported for a microphone that is
        // not actually recording. It costs the loop a few milliseconds and
        // buys a synchronous, actionable error for the CLI's exit code.
        if let Err(e) = self.pipeline.audio.start().await {
            warn!("failed to start recording: {e}");
            self.pipeline
                .notifier
                .notify(crate::ports::Notice::Error(format!("Recording failed: {e}")));
            return Err(ProtoError::new(ErrorCode::AudioDeviceError, e.to_string()));
        }

        let token = CancelToken::new();
        let id = new_session_id();
        let handle = SessionHandle::new(id.clone(), mode.clone(), token, self.bus.clone());
        if let Err(e) = handle.advance(State::Recording) {
            // Unreachable: a fresh handle is always `idle`. Release the device
            // rather than leaving a stream running for a session that does not
            // exist.
            self.pipeline.audio.cancel();
            return Err(ProtoError::new(ErrorCode::Internal, e.to_string()));
        }

        let stop = Arc::new(Notify::new());
        self.active = Some(ActiveSession {
            handle: handle.clone(),
            owner: actor.as_owner(),
            stop: stop.clone(),
            stop_issued: false,
        });

        let pipeline = self.pipeline.clone();
        let tx = self.tx.clone();
        let session_id = id.clone();
        tokio::spawn(async move {
            let outcome = pipeline.run(handle, stop, options).await;
            // Report back through the same mailbox rather than mutating engine
            // state from this task — the single-writer rule holds for the
            // pipeline too.
            let _ = tx
                .send(EngineRequest::SessionFinished {
                    session_id,
                    outcome: Box::new(outcome),
                })
                .await;
        });

        info!(session = %id.as_str(), ?mode, "session started");
        Ok(id)
    }

    fn handle_stop(&mut self, actor: &Actor) -> Result<SessionId, ProtoError> {
        let session = self.authorize(actor)?;
        if session.stop_issued {
            return Err(ProtoError::new(
                ErrorCode::InvalidState,
                "this session has already left the recording phase",
            ));
        }
        session.stop_issued = true;
        // `notify_one`, not `notify_waiters`: the session task may not have
        // reached its park yet (it is still raising the recording notice and
        // pausing media), and `notify_waiters` would drop the signal on the
        // floor, hanging the session forever. `notify_one` stores a permit the
        // task collects whenever it arrives.
        session.stop.notify_one();
        let id = session.handle.id().clone();
        info!(session = %id.as_str(), "stop requested");
        Ok(id)
    }

    fn handle_cancel(&mut self, actor: &Actor) -> Result<SessionId, ProtoError> {
        let session = self.authorize(actor)?;
        let id = session.handle.id().clone();
        match session.handle.token().cancel() {
            CancelVerdict::Accepted | CancelVerdict::AlreadyCancelled => {
                // No `stop` notification here: the token wakes the session's
                // select directly, and storing a stop permit would leave the
                // task ready to treat a cancelled session as a stopped one.
                info!(session = %id.as_str(), "cancel accepted");
                Ok(id)
            }
            CancelVerdict::TooLate => {
                // The honest answer. Reporting success here would tell the
                // client nothing was typed while text was already landing in
                // their editor.
                warn!(session = %id.as_str(), "cancel arrived after the injection commit point");
                Err(ProtoError::new(
                    ErrorCode::Conflict,
                    "injection has already been committed; this session cannot be cancelled",
                ))
            }
        }
    }

    async fn handle_toggle(
        &mut self,
        actor: &Actor,
        options: ResolvedOptions,
    ) -> Result<ToggleOutcome, ProtoError> {
        // Resolved *inside* the engine, so there is no window between reading
        // the state and acting on it. A client doing `get_status` then
        // `start_dictation` has that window and gets `busy` if it loses; the
        // signal path does not, which is why it uses this.
        match self.active.as_ref().map(|s| s.handle.state()) {
            None => self
                .handle_start(actor, DictationMode::Toggle, options)
                .await
                .map(ToggleOutcome::Started),
            Some(state) if state.is_terminal() => self
                .handle_start(actor, DictationMode::Toggle, options)
                .await
                .map(ToggleOutcome::Started),
            Some(State::Recording) => self.handle_stop(actor).map(ToggleOutcome::Stopped),
            Some(other) => Err(ProtoError::new(
                ErrorCode::Busy,
                format!(
                    "session is {}; wait for it to finish or cancel it",
                    other.as_str()
                ),
            )
            .with_retry_after_ms(500)),
        }
    }

    /// Resolve the active session and check the caller may control it.
    ///
    /// Ordering matters: "is there a session" is answered before "may you
    /// touch it", so a caller with no session at all gets `no_active_session`
    /// rather than a confusing `forbidden`.
    fn authorize(&mut self, actor: &Actor) -> Result<&mut ActiveSession, ProtoError> {
        let Some(session) = self.active.as_mut() else {
            return Err(ProtoError::new(
                ErrorCode::NoActiveSession,
                "no dictation session is running",
            ));
        };
        if session.handle.is_finished() {
            return Err(ProtoError::new(
                ErrorCode::NoActiveSession,
                "no dictation session is running",
            ));
        }
        if !session.owner.may_control(actor) {
            return Err(ProtoError::new(
                ErrorCode::Forbidden,
                "this session belongs to another client",
            ));
        }
        Ok(session)
    }

    fn handle_disconnected(&mut self, client: ClientId, host_capture: bool) {
        let Some(session) = self.active.as_mut() else {
            return;
        };
        match session.owner.on_disconnect(client, host_capture) {
            DisconnectAction::Ignore => {}
            DisconnectAction::Orphan => {
                // The `dictate toggle` case: the process that started the
                // session has exited, but the *user* has not gone anywhere.
                info!(
                    session = %session.handle.id().as_str(),
                    %client,
                    "owner disconnected; session handed to the host"
                );
                session.owner = SessionOwner::Host;
            }
            DisconnectAction::Cancel => {
                info!(
                    session = %session.handle.id().as_str(),
                    %client,
                    "owner disconnected with no one to take delivery; cancelling"
                );
                session.handle.token().cancel();
            }
        }
    }

    fn handle_session_finished(&mut self, session_id: &SessionId) {
        let Some(session) = self.active.as_ref() else {
            return;
        };
        if session.handle.id() != session_id {
            // A late report from a session that has already been replaced.
            debug!(session = %session_id.as_str(), "ignoring stale completion");
            return;
        }
        let terminal = session.handle.state();
        let id = session.handle.id().clone();
        self.active = None;

        // Protocol rule 2: a terminal state may only reset to idle. Emitting
        // it explicitly is what lets a HUD clear itself without polling.
        if terminal.is_terminal() {
            self.bus.publish(Event::StateChanged {
                session_id: id,
                from: terminal,
                to: State::Idle,
                at_ms: Some(now_ms()),
            });
        }
    }

    fn handle_shutdown(&mut self) {
        if let Some(session) = self.active.as_ref() {
            info!("cancelling in-flight session for shutdown");
            session.handle.token().cancel();
        }
        self.pipeline.audio.cancel();
    }

    fn status(&self, capabilities: Capabilities) -> Status {
        let session = self.active.as_ref().filter(|s| !s.handle.is_finished());
        let model = self.pipeline.stt.model();
        Status {
            state: session.map_or(State::Idle, |s| s.handle.state()),
            session: session.map(|s| SessionSummary {
                session_id: s.handle.id().clone(),
                state: s.handle.state(),
                mode: s.handle.mode(),
                started_ms: Some(s.handle.started_ms()),
                route: None,
            }),
            daemon: DaemonInfo {
                name: self.identity.name.clone(),
                version: self.identity.version.clone(),
                protocol_version: dictate_proto::PROTOCOL_VERSION,
                pid: Some(std::process::id()),
                uptime_ms: Some(now_ms().saturating_sub(self.started_ms)),
            },
            model: Some(ModelStatus {
                name: model.name,
                loaded: model.loaded,
                backend: model.backend,
            }),
            capabilities,
        }
    }
}

/// Resolve a command's [`SessionOptions`] against the caller's capabilities.
///
/// Shared by the socket and the signal shim so both reach the pipeline with
/// options resolved the same way.
///
/// Two rules from the protocol are enforced here:
///
/// - an **omitted** field means "use the server's default", never "off" — so
///   `inject: None` resolves from capabilities rather than to `false`;
/// - asking for injection **without** `text_injection` is `forbidden`, not a
///   silent downgrade. A client that believes it is dictating into an editor
///   must be told it is not.
///
/// # Errors
///
/// `forbidden` when the caller asks for something its capabilities do not
/// grant.
pub fn resolve_options(
    options: Option<&SessionOptions>,
    capabilities: &Capabilities,
) -> Result<ResolvedOptions, ProtoError> {
    let default = SessionOptions::default();
    let options = options.unwrap_or(&default);

    let inject = match options.inject {
        Some(true) if !capabilities.features.text_injection => {
            return Err(ProtoError::new(
                ErrorCode::Forbidden,
                "this connection may not inject text",
            ));
        }
        Some(explicit) => explicit,
        None => capabilities.features.text_injection,
    };

    if let Some(route) = &options.route {
        if !capabilities.allows_route(route) {
            return Err(ProtoError::new(
                ErrorCode::Forbidden,
                format!("route '{}' is not permitted for this connection", route.as_str()),
            ));
        }
    }

    Ok(ResolvedOptions {
        inject,
        forced_route: options.route.clone(),
        allowed_routes: capabilities.routes.clone(),
        privacy: options.privacy.unwrap_or(false),
        app: options.app.clone(),
    })
}

/// Whether a command needs a session to exist, used to answer `get_status`-only
/// clients without touching the engine.
#[must_use]
pub fn is_session_command(command: &Command) -> bool {
    matches!(command, Command::Stop | Command::Cancel)
}

/// Every route a trusted local connection may invoke.
#[must_use]
pub fn local_routes() -> Vec<Route> {
    Route::known().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dictate_proto::Features;

    fn caps(text_injection: bool) -> Capabilities {
        let mut c = Capabilities::local_trusted();
        c.features.text_injection = text_injection;
        c
    }

    #[test]
    fn omitted_inject_follows_capabilities() {
        let resolved = resolve_options(None, &caps(true)).unwrap();
        assert!(resolved.inject, "a local client injects by default");

        let resolved = resolve_options(None, &Capabilities::remote_transcription_only()).unwrap();
        assert!(
            !resolved.inject,
            "a remote client takes delivery by default rather than typing on the host"
        );
    }

    #[test]
    fn asking_to_inject_without_permission_is_forbidden_not_downgraded() {
        let options = SessionOptions {
            inject: Some(true),
            ..SessionOptions::default()
        };
        let err = resolve_options(Some(&options), &caps(false)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[test]
    fn asking_not_to_inject_is_always_honored() {
        let options = SessionOptions {
            inject: Some(false),
            ..SessionOptions::default()
        };
        assert!(!resolve_options(Some(&options), &caps(true)).unwrap().inject);
    }

    #[test]
    fn a_forced_route_is_gated_by_capabilities() {
        let options = SessionOptions {
            route: Some(Route::Timer),
            ..SessionOptions::default()
        };
        // Deny-by-default: `remote_transcription_only` lists only `type`.
        let err = resolve_options(Some(&options), &Capabilities::remote_transcription_only())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);

        assert_eq!(
            resolve_options(Some(&options), &Capabilities::local_trusted())
                .unwrap()
                .forced_route,
            Some(Route::Timer)
        );
    }

    #[test]
    fn an_empty_route_list_permits_nothing() {
        // The S01 overrule (`5f15051`), asserted from the daemon's side.
        let mut c = Capabilities::local_trusted();
        c.routes.clear();
        let options = SessionOptions {
            route: Some(Route::Type),
            ..SessionOptions::default()
        };
        assert_eq!(
            resolve_options(Some(&options), &c).unwrap_err().code,
            ErrorCode::Forbidden
        );
        assert!(resolve_options(None, &c).unwrap().allowed_routes.is_empty());
    }

    #[test]
    fn privacy_defaults_off_and_is_honored_when_asked() {
        assert!(!resolve_options(None, &caps(true)).unwrap().privacy);
        let options = SessionOptions {
            privacy: Some(true),
            ..SessionOptions::default()
        };
        assert!(resolve_options(Some(&options), &caps(true)).unwrap().privacy);
    }

    #[test]
    fn local_trusted_features_include_host_capture() {
        // The discriminator session ownership depends on.
        assert!(Features::local_trusted().host_capture);
        assert!(!Features::remote_transcription_only().host_capture);
    }

    #[test]
    fn session_commands_are_the_ones_without_a_session_id() {
        assert!(is_session_command(&Command::Stop));
        assert!(is_session_command(&Command::Cancel));
        assert!(!is_session_command(&Command::GetStatus));
    }
}
