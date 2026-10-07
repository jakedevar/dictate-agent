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
//! same channel the protocol commands use. [`Engine::handle_toggle`] is shared
//! by signals and protocol `toggle` requests, so there is no path by which the
//! two toggle routes can drift apart.

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
    SuppliedAudio,
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

/// The claim ticket for an uploaded-audio session: the session's id, and a
/// channel that delivers its outcome once the pipeline settles.
///
/// A transcription request's response *is* the transcript, so unlike a live
/// dictation (whose result arrives as a `final` event) the caller has to wait
/// for the outcome. Handing it out as a ticket keeps that wait out of the
/// engine's mailbox loop.
pub struct TranscribeTicket {
    /// The session running the upload.
    pub session_id: SessionId,
    /// Resolves when the session reaches a terminal state.
    pub outcome: oneshot::Receiver<PipelineOutcome>,
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
    /// Begin a session that transcribes audio the caller supplied — no
    /// microphone, media pause or earcons.
    Transcribe {
        /// Who asked.
        actor: Actor,
        /// The decoded 16 kHz mono audio.
        audio: SuppliedAudio,
        /// Options, resolved by [`resolve_upload_options`].
        options: ResolvedOptions,
        /// Where to send the ticket (or the refusal).
        reply: oneshot::Sender<Result<TranscribeTicket, ProtoError>>,
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
    /// Discover current focus for a capability-checked local connection.
    GetContext {
        /// Resolved profile, including None on a headless host.
        reply: oneshot::Sender<dictate_proto::ResolvedProfile>,
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
    /// Discovery only: callers must enforce Features::context_read.
    pub async fn get_context(&self) -> Result<dictate_proto::ResolvedProfile, ProtoError> {
        self.ask(|reply| EngineRequest::GetContext { reply }).await
    }
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

    /// Start a session that transcribes `audio` instead of capturing.
    ///
    /// Runs through the same single-writer slot as a live dictation, so a busy
    /// engine answers `busy` and `cancel` works on it.
    ///
    /// # Errors
    ///
    /// `busy` if a session is already running.
    pub async fn transcribe(
        &self,
        actor: Actor,
        audio: SuppliedAudio,
        options: ResolvedOptions,
    ) -> Result<TranscribeTicket, ProtoError> {
        self.ask(|reply| EngineRequest::Transcribe {
            actor,
            audio,
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
        self.ask(|reply| EngineRequest::Stop { actor, reply })
            .await?
    }

    /// Cancel the active session.
    ///
    /// # Errors
    ///
    /// `no_active_session`, `forbidden` if the caller does not own it, or
    /// `conflict` if injection has already been committed.
    pub async fn cancel(&self, actor: Actor) -> Result<SessionId, ProtoError> {
        self.ask(|reply| EngineRequest::Cancel { actor, reply })
            .await?
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
    /// Live context options; uploads never have a host focus decision.
    context_options: Option<ResolvedOptions>,
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
    pub fn new(
        pipeline: Arc<Pipeline>,
        bus: EventBus,
        identity: DaemonIdentity,
    ) -> (Self, EngineHandle) {
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
                EngineRequest::Transcribe {
                    actor,
                    audio,
                    options,
                    reply,
                } => {
                    let _ = reply.send(self.handle_transcribe(&actor, audio, options));
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
                EngineRequest::GetContext { reply } => {
                    let _ = reply.send(self.pipeline.context.resolve(None, true));
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
        mut options: ResolvedOptions,
    ) -> Result<SessionId, ProtoError> {
        // `max_concurrent_sessions` is 1. Two racing starts are not a data
        // race — they are simply ordered by the mailbox, and the second one
        // sees the first one's session and is told to back off.
        if self.has_live_session() {
            return Err(
                ProtoError::new(ErrorCode::Busy, "a dictation session is already running")
                    .with_retry_after_ms(500),
            );
        }

        // Audio-less mode is a choice the user made, not a device fault: say so
        // plainly (and on the desktop, where a hotkey press has no terminal).
        if let Some(reason) = self.pipeline.audio.unavailable_reason() {
            self.pipeline.notifier.notify(crate::ports::Notice::Error(
                "Audio capture is disabled ([audio] capture = false)".into(),
            ));
            return Err(ProtoError::new(ErrorCode::CapabilityUnavailable, reason));
        }

        // Capture on acceptance, before the first await can let focus move.
        self.pipeline.resolve_context(&mut options);

        // Opening the device is awaited here, inside the serialized loop, so
        // that `session_started` is never reported for a microphone that is
        // not actually recording. It costs the loop a few milliseconds and
        // buys a synchronous, actionable error for the CLI's exit code.
        if let Err(e) = self.pipeline.audio.start().await {
            warn!("failed to start recording: {e}");
            self.pipeline
                .notifier
                .notify(crate::ports::Notice::Error(format!(
                    "Recording failed: {e}"
                )));
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
            context_options: Some(options.clone()),
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

    /// Start an upload session. Deliberately synchronous: nothing here awaits a
    /// device, so the slot is claimed and the pipeline spawned in one turn of
    /// the mailbox, and a second request can only ever see the first's session.
    fn handle_transcribe(
        &mut self,
        actor: &Actor,
        audio: SuppliedAudio,
        mut options: ResolvedOptions,
    ) -> Result<TranscribeTicket, ProtoError> {
        if self.has_live_session() {
            return Err(
                ProtoError::new(ErrorCode::Busy, "a dictation session is already running")
                    .with_retry_after_ms(500),
            );
        }

        // The audio did not come from this desktop, so host focus is never
        // read for an upload; only an app the caller named selects a profile
        // and the dictionary's per-app scope.
        options.capture_context = false;
        self.pipeline.resolve_context(&mut options);

        let token = CancelToken::new();
        let id = new_session_id();
        let handle = SessionHandle::new_upload(id.clone(), token, self.bus.clone(), audio);

        // There is no recording phase to wait out: store the stop permit now so
        // the pipeline's wait passes straight through (a cancel still wins —
        // the wait is biased toward it). `stop_issued` makes a client `stop`
        // an honest `invalid_state` rather than a silent no-op.
        let stop = Arc::new(Notify::new());
        stop.notify_one();
        self.active = Some(ActiveSession {
            handle: handle.clone(),
            owner: actor.as_upload_owner(),
            stop: stop.clone(),
            stop_issued: true,
            context_options: None,
        });

        let (outcome_tx, outcome_rx) = oneshot::channel();
        let pipeline = self.pipeline.clone();
        let tx = self.tx.clone();
        let session_id = id.clone();
        tokio::spawn(async move {
            let outcome = pipeline.run(handle, stop, options).await;
            // Free the engine's slot *before* answering the caller, so a client
            // that fires its next request the moment it has a transcript is
            // ordered after the release by the mailbox instead of racing it.
            let _ = tx
                .send(EngineRequest::SessionFinished {
                    session_id,
                    outcome: Box::new(outcome.clone()),
                })
                .await;
            let _ = outcome_tx.send(outcome);
        });

        info!(session = %id.as_str(), "upload session started");
        Ok(TranscribeTicket {
            session_id: id,
            outcome: outcome_rx,
        })
    }

    fn handle_stop(&mut self, actor: &Actor) -> Result<SessionId, ProtoError> {
        let context = self.pipeline.context.clone();
        let injector = self.pipeline.injector.clone();
        let session = self.authorize(actor)?;
        if session.stop_issued {
            return Err(ProtoError::new(
                ErrorCode::InvalidState,
                "this session has already left the recording phase",
            ));
        }
        if let Some(options) = &session.context_options {
            if let Some(binding) = &options.stop_destination {
                *binding.lock().expect("stop destination poisoned") =
                    Some(injector.capture_destination());
            }
            // This bounded capture runs at the user's stop action, before the
            // pipeline wakes. Named apps and untrusted callers do not capture.
            session
                .handle
                .set_stop_profile(context.resolve(options.app.as_deref(), options.capture_context));
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
        // Resolved inside the engine, so there is no window between reading
        // the state and acting on it. Both protocol `toggle` and SIGUSR1 use
        // this mailbox operation rather than a `get_status` then action pair.
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
            formatter: self.pipeline.formatter.status(),
            audio: self.pipeline.audio.input_status(),
        }
    }
}

/// Resolve options for an uploaded-audio session.
///
/// The one rule that differs from [`resolve_options`]: an omitted `inject`
/// never means "the connection's default". A live dictation types into the
/// focused window because that is what a hotkey is *for*; an upload is a
/// request/response call, and the caller who did not ask for typing must not
/// have text appear in whatever window happens to have focus. Asking for it
/// (`Some(true)`) still requires the `text_injection` capability.
///
/// # Errors
///
/// `forbidden` when the caller asks for something its capabilities do not
/// grant.
pub fn resolve_upload_options(
    options: Option<&SessionOptions>,
    capabilities: &Capabilities,
) -> Result<ResolvedOptions, ProtoError> {
    let mut resolved = resolve_options(options, capabilities)?;
    resolved.inject = options.is_some_and(|o| o.inject == Some(true));
    resolved.stop_destination = None;
    resolved.capture_context = false;
    Ok(resolved)
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
                format!(
                    "route '{}' is not permitted for this connection",
                    route.as_str()
                ),
            ));
        }
    }

    Ok(ResolvedOptions {
        stop_destination: (inject && capabilities.features.host_capture)
            .then(|| Arc::new(std::sync::Mutex::new(None))),
        inject,
        forced_route: options.route.clone(),
        allowed_routes: capabilities.routes.clone(),
        privacy: options.privacy.unwrap_or(false),
        use_dictionary: options.use_dictionary.unwrap_or(true),
        format_llm: options.format_llm,
        app: options.app.clone(),
        capture_context: capabilities.features.context_read && capabilities.features.host_capture,
        context: None,
        profile: Default::default(),
    })
}

/// Whether a command needs a session to exist, used to answer `get_status`-only
/// clients without touching the engine.
#[must_use]
pub fn is_session_command(command: &Command) -> bool {
    matches!(command, Command::Toggle | Command::Stop | Command::Cancel)
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

    /// The real engine/pipeline run with synthetic window identities. The
    /// injector refuses delivery when processing outlives the stop window.
    #[tokio::test]
    async fn explicit_stop_binds_delivery_and_records_clipboard_only_outcome() {
        use crate::ports::{mock::*, BoxFuture, Notice, TextInjector};
        use dictate_proto::InjectionOutcome;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Mutex;

        struct BoundInjector(AtomicU32);
        impl TextInjector for BoundInjector {
            fn inject(
                &self,
                _: &str,
                _: Option<dictate_inject::InjectionPolicy>,
            ) -> BoxFuture<'_, InjectionOutcome> {
                panic!("live delivery must use the bound injector seam")
            }
            fn capture_destination(&self) -> Option<u32> {
                Some(self.0.load(Ordering::SeqCst))
            }
            fn inject_bound(
                &self,
                _: &str,
                _: Option<dictate_inject::InjectionPolicy>,
                destination: Option<Option<u32>>,
            ) -> BoxFuture<'_, InjectionOutcome> {
                assert_eq!(
                    destination,
                    Some(Some(101)),
                    "window at stop, before pipeline wakes"
                );
                assert_eq!(
                    self.capture_destination(),
                    Some(202),
                    "focus moved while processing"
                );
                Box::pin(async {
                    InjectionOutcome::Failed {
                        error: ProtoError::new(
                            ErrorCode::InjectionFailed,
                            "Dictation copied: focus changed",
                        ),
                    }
                })
            }
            fn is_available(&self) -> bool {
                true
            }
        }

        for privacy in [false, true] {
            let dir = std::path::Path::new("/tmp")
                .join(format!("dictate-binding-{}", uuid::Uuid::new_v4()));
            let history = Arc::new(Mutex::new(
                dictate_history::HistoryStore::new(&dictate_history::HistoryConfig {
                    db_path: dir.join("history.db").to_string_lossy().into_owned(),
                    ..Default::default()
                })
                .unwrap(),
            ));
            let injector = Arc::new(BoundInjector(AtomicU32::new(101)));
            let notifier = Arc::new(RecordingNotifier::default());
            let pipeline = Arc::new(Pipeline {
                context: Arc::new(dictate_context::ContextEngine::disabled()),
                audio: Arc::new(MockAudio::with_seconds(1.0)),
                stt: Arc::new(MockStt::returning("synthetic dictation")),
                dictionary: None,
                vad: Arc::new(
                    dictate_vad::SileroVad::new(dictate_vad::VadConfig {
                        enabled: false,
                        ..Default::default()
                    })
                    .unwrap(),
                ),
                text_chain: Arc::new(dictate_fmt::TextChain::default()),
                formatter: Arc::new(MockFormatter::default()),
                injector: injector.clone(),
                notifier: notifier.clone(),
                media: Arc::new(NullMedia),
                earcons: Arc::new(NullEarcons),
                history: history.clone(),
                local: Arc::new(crate::local_executor::LocalExecutor::new(
                    &Default::default(),
                )),
                timer: Arc::new(crate::timer::TimerExecutor::new(&Default::default())),
                local_model: "synthetic".into(),
            });
            let (mut engine, _handle) =
                Engine::new(pipeline, EventBus::new(64), Default::default());
            let opts = resolve_options(
                Some(&SessionOptions {
                    privacy: Some(privacy),
                    ..Default::default()
                }),
                &Capabilities::local_trusted(),
            )
            .unwrap();
            let binding = opts.stop_destination.clone().unwrap();
            engine
                .handle_start(&Actor::Signal, DictationMode::Toggle, opts)
                .await
                .unwrap();
            engine.handle_stop(&Actor::Signal).unwrap();
            assert_eq!(*binding.lock().unwrap(), Some(Some(101)));
            injector.0.store(202, Ordering::SeqCst);
            let finished =
                tokio::time::timeout(std::time::Duration::from_secs(5), engine.rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
            let EngineRequest::SessionFinished { outcome, .. } = finished else {
                panic!("terminal outcome");
            };
            assert!(
                matches!(outcome.transcript.unwrap().injection, InjectionOutcome::Failed { ref error }
                if error.message == "Dictation copied: focus changed")
            );
            assert!(notifier.notices().contains(&Notice::InjectionFailed(
                "Dictation copied: focus changed".into()
            )));
            assert!(
                !notifier
                    .notices()
                    .iter()
                    .any(|n| matches!(n, Notice::Error(_))),
                "ordinary error notifier overwrites clipboard"
            );
            let store = history.lock().unwrap();
            let count: i64 = store
                .connection()
                .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, if privacy { 0 } else { 1 });
            if !privacy {
                let row: (bool, String) = store
                    .connection()
                    .query_row(
                        "SELECT output_typed, error_summary FROM interactions",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(row, (false, "Dictation copied: focus changed".into()));
            }
            drop(store);
            drop(engine);
            drop(history);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    fn caps(text_injection: bool) -> Capabilities {
        let mut c = Capabilities::local_trusted();
        c.features.text_injection = text_injection;
        c
    }

    /// `capture_context = context_read && host_capture`: a session reads the
    /// host's focused window only for a connection that may both see context
    /// and has the host's input focus to capture. Every combination.
    #[test]
    fn capture_context_requires_both_context_read_and_host_capture() {
        for (context_read, host_capture) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let mut c = Capabilities::local_trusted();
            c.features.context_read = context_read;
            c.features.host_capture = host_capture;
            let want = context_read && host_capture;
            assert_eq!(
                resolve_options(None, &c).unwrap().capture_context,
                want,
                "context_read={context_read} host_capture={host_capture}"
            );
            // A session start does not widen it, and uploads never capture.
            let options = SessionOptions {
                app: Some("slack".into()),
                ..SessionOptions::default()
            };
            assert_eq!(
                resolve_options(Some(&options), &c).unwrap().capture_context,
                want
            );
            assert!(
                resolve_upload_options(
                    Some(&SessionOptions {
                        inject: Some(true),
                        ..Default::default()
                    }),
                    &Capabilities::local_trusted()
                )
                .unwrap()
                .stop_destination
                .is_none(),
                "uploads never bind to host focus"
            );
            assert!(
                !resolve_upload_options(None, &c).unwrap().capture_context,
                "uploads never read host focus"
            );
        }
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
    fn an_upload_never_injects_unless_it_asks() {
        // Even on a connection that may inject, silence means "return the text".
        let local = caps(true);
        assert!(!resolve_upload_options(None, &local).unwrap().inject);
        let bare = SessionOptions::default();
        assert!(!resolve_upload_options(Some(&bare), &local).unwrap().inject);
        let no = SessionOptions {
            inject: Some(false),
            ..SessionOptions::default()
        };
        assert!(!resolve_upload_options(Some(&no), &local).unwrap().inject);

        let yes = SessionOptions {
            inject: Some(true),
            ..SessionOptions::default()
        };
        assert!(resolve_upload_options(Some(&yes), &local).unwrap().inject);
    }

    #[test]
    fn an_upload_that_asks_to_inject_without_permission_is_forbidden() {
        let yes = SessionOptions {
            inject: Some(true),
            ..SessionOptions::default()
        };
        let err = resolve_upload_options(Some(&yes), &caps(false)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        let err = resolve_upload_options(Some(&yes), &Capabilities::remote_transcription_only())
            .unwrap_err();
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
        assert!(
            resolve_options(Some(&options), &caps(true))
                .unwrap()
                .privacy
        );
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
        assert!(is_session_command(&Command::Toggle));
        assert!(!is_session_command(&Command::GetStatus));
    }
}
