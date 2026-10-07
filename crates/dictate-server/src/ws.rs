//! One WebSocket session: dictate-proto envelopes as text frames, answered by
//! the daemon's own line handling, with the connection's scoped events
//! interleaved.
//!
//! # Shape
//!
//! A reader task owns the receive half and queues text frames (bounded); the
//! session loop owns the send half and the daemon connection, so replies and
//! events are written one at a time — the same single-writer ordering the unix
//! socket has: a response is always written before the events it caused.
//!
//! # Every request is admitted again
//!
//! The upgrade passed the HTTP gate once; the session does not live on that.
//! Before each command runs:
//!
//! - its size is held to the connection's *current* limit — the socket's
//!   pre-handshake ceiling until the handshake, the grant's after — before a
//!   byte of it is parsed; a longer one is answered `payload_too_large` and
//!   closes the session (1009), as on the socket;
//! - the peer's throttle bucket must hold a token. A peer over its rate waits
//!   (at most [`MAX_THROTTLE_WAIT`]) and asks again; a wait that would run
//!   longer, a lockout, or a saturated table is answered `rate_limited` and the
//!   command does not run;
//! - the token the session authenticated with must still be the current one.
//!   Rotating, deleting or loosening the token file closes the session (1008);
//!   an idle session finds out within [`REVALIDATE`].
//!
//! # Bounds
//!
//! - Frame and message size: the upgrade buffers at most
//!   [`Backend::max_message_bytes`](crate::Backend::max_message_bytes).
//! - Pipelining: at most [`PIPELINE`] requests queue behind the one in flight;
//!   one more closes the connection (1008) rather than buffering without end.
//! - Binary frames: refused (1003). Audio streaming is not implemented in this
//!   build (design §11); whole clips go as `transcribe_audio`.
//! - Time: every await is bounded. A request (admission, execution and reply)
//!   must finish within [`IDLE`] of arriving (else 1011); a session with no
//!   client message for [`IDLE`] closes (1000); a write a peer does not read
//!   for [`SEND_TIMEOUT`] ends the session; the close handshake gets
//!   [`CLOSE_TIMEOUT`]. Server shutdown interrupts all of it (1001), and
//!   [`WsTasks::shutdown`] aborts any session that does not finish closing.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket};
use dictate_proto::{ErrorCode, Event, Message, ProtoError, RequestId};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tracing::{debug, info};

use crate::backend::{Hangup, Session};
use crate::http::{throttled_error, AppState};
use crate::limit::Throttled;
use crate::token::Credential;

/// Requests that may queue behind the one being answered.
pub(crate) const PIPELINE: usize = 4;
/// A session with no client message for this long is closed, and a request
/// must be answered within this of arriving.
pub(crate) const IDLE: Duration = Duration::from_secs(300);
/// Longest a request waits for the peer's rate to admit it; past this it is
/// refused, never run.
pub(crate) const MAX_THROTTLE_WAIT: Duration = Duration::from_secs(5);
/// Longest one write (a reply or an event) may wait for a peer to read.
pub(crate) const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest the close frame, and then closing the socket, may each take.
pub(crate) const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often a session that sends nothing re-checks its token.
pub(crate) const REVALIDATE: Duration = Duration::from_secs(2);

type Sink = SplitSink<WebSocket, WsMessage>;

/// Why the reader stopped on its own.
#[derive(Debug, Clone, Copy)]
enum ReaderStop {
    /// The peer pipelined more than [`PIPELINE`] requests.
    Overloaded,
    /// The peer sent a binary frame.
    Binary,
}

/// The upgraded WebSocket sessions.
///
/// Hyper runs an upgrade's callback in a task nothing else can reach, so the
/// callback moves the session here instead; [`WsTasks::shutdown`] can then
/// wait for every session to close, and abort (dropping its daemon
/// connection, which cancels what it owned) any that does not.
pub(crate) struct WsTasks {
    /// `None` once shutdown has begun: a session upgraded after that is
    /// dropped, not started.
    set: Mutex<Option<JoinSet<()>>>,
}

impl WsTasks {
    pub(crate) fn new() -> Self {
        Self {
            set: Mutex::new(Some(JoinSet::new())),
        }
    }

    /// Run `session`, unless shutdown has begun.
    pub(crate) fn spawn(&self, session: impl Future<Output = ()> + Send + 'static) {
        let Ok(mut guard) = self.set.lock() else {
            return;
        };
        let Some(set) = guard.as_mut() else {
            debug!("WebSocket upgraded during shutdown; dropping it");
            return;
        };
        // Reap finished sessions, so the set holds only live ones.
        while set.try_join_next().is_some() {}
        set.spawn(session);
    }

    /// Wait up to `grace` for every session to end, then abort the rest and
    /// wait for them to unwind.
    pub(crate) async fn shutdown(&self, grace: Duration) {
        let taken = self.set.lock().ok().and_then(|mut guard| guard.take());
        let Some(mut set) = taken else {
            return;
        };
        let _ =
            tokio::time::timeout(grace, async { while set.join_next().await.is_some() {} }).await;
        set.shutdown().await;
    }
}

pub(crate) async fn run(
    socket: WebSocket,
    state: Arc<AppState>,
    peer: SocketAddr,
    credential: Credential,
) {
    info!(peer = %peer.ip(), "network API WebSocket session opened");
    let (mut sink, mut stream) = socket.split();
    let (queue, mut requests) = mpsc::channel::<String>(PIPELINE);
    let (gone_tx, gone) = watch::channel(false);

    let mut reader = tokio::spawn(async move {
        let stop = loop {
            match stream.next().await {
                Some(Ok(WsMessage::Text(text))) => match queue.try_send(text.to_string()) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => break Some(ReaderStop::Overloaded),
                    Err(mpsc::error::TrySendError::Closed(_)) => break None,
                },
                Some(Ok(WsMessage::Binary(_))) => break Some(ReaderStop::Binary),
                Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
                Some(Ok(WsMessage::Close(_)) | Err(_)) | None => break None,
            }
        };
        let _ = gone_tx.send(true);
        stop
    });

    let mut session = state.backend.open();
    let mut closing = state.closing.clone();
    let mut deadline = Instant::now() + IDLE;
    let mut revalidate = tokio::time::interval_at(Instant::now() + REVALIDATE, REVALIDATE);
    revalidate.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let close: Option<CloseFrame> = loop {
        tokio::select! {
            request = requests.recv() => {
                let Some(text) = request else { break None };
                if *gone.borrow() {
                    // Nobody is left to read the answer.
                    break None;
                }
                deadline = Instant::now() + IDLE;
                let work = answer(&mut sink, session.as_mut(), &state, peer, &credential, &text, &gone);
                match bounded(&mut closing, deadline, work).await {
                    Bounded::Done(Answered::Next) => {}
                    Bounded::Done(Answered::Close(frame)) => break Some(frame),
                    Bounded::Done(Answered::Gone) => break None,
                    Bounded::Closing => break Some(server_closing()),
                    Bounded::Expired => break Some(frame(
                        1011,
                        "the request did not finish within the session's time limit",
                    )),
                }
            }
            event = session.next_event() => match event {
                Some(event) => {
                    if !state.tokens.still_valid(&credential) {
                        break Some(revoked());
                    }
                    let message = Message::event(event);
                    let write = send(&mut sink, &message);
                    match bounded(&mut closing, Instant::now() + SEND_TIMEOUT, write).await {
                        Bounded::Done(Ok(())) => {}
                        Bounded::Done(Err(())) | Bounded::Expired => break None,
                        Bounded::Closing => break Some(server_closing()),
                    }
                }
                None => break Some(frame(1001, "the daemon is shutting down")),
            },
            () = tokio::time::sleep_until(deadline) => break Some(frame(1000, "idle")),
            () = closed(&mut closing) => break Some(server_closing()),
            _ = revalidate.tick() => {
                if !state.tokens.still_valid(&credential) {
                    break Some(revoked());
                }
            }
        }
    };

    let close = match close {
        Some(frame) => Some(frame),
        // The reader stopped (or the peer stopped reading): its reason, if
        // it has one and gives it promptly.
        None => match tokio::time::timeout(CLOSE_TIMEOUT, &mut reader).await {
            Ok(Ok(Some(ReaderStop::Overloaded))) => {
                Some(frame(1008, "too many pipelined requests"))
            }
            Ok(Ok(Some(ReaderStop::Binary))) => Some(frame(
                1003,
                "binary audio frames are not supported by this server; send transcribe_audio",
            )),
            _ => None,
        },
    };
    reader.abort();
    if let Some(frame) = close {
        debug!(peer = %peer.ip(), code = frame.code, reason = %frame.reason, "closing WebSocket");
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, sink.send(WsMessage::Close(Some(frame)))).await;
    }
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, sink.close()).await;
    // Dropping the daemon connection is the disconnect: an upload this
    // session still owns is cancelled.
    drop(session);
    info!(peer = %peer.ip(), "network API WebSocket session closed");
}

/// What answering one request left the session to do.
enum Answered {
    /// Read the next request.
    Next,
    /// Close the session with this frame.
    Close(CloseFrame),
    /// The peer cannot be written to; end without a close frame.
    Gone,
}

/// Admit, run and answer one request.
async fn answer(
    sink: &mut Sink,
    session: &mut dyn Session,
    state: &AppState,
    peer: SocketAddr,
    credential: &Credential,
    text: &str,
    gone: &watch::Receiver<bool>,
) -> Answered {
    // The connection's own ceiling, before a byte of it is parsed.
    let limit = session.max_message_bytes();
    if text.len() > limit {
        let error = ProtoError::new(
            ErrorCode::PayloadTooLarge,
            format!("message exceeds the {limit}-byte limit for this connection"),
        );
        let _ = send(
            sink,
            &Message::event(Event::Error {
                session_id: None,
                error,
            }),
        )
        .await;
        return Answered::Close(frame(1009, "message too big for this connection"));
    }

    if let Err(throttled) = admit(state, peer).await {
        debug!(peer = %peer.ip(), what = throttled.as_str(), "network API throttled a WebSocket request");
        let reply = refusal(text, throttled_error(throttled));
        return match send(sink, &reply).await {
            Ok(()) => Answered::Next,
            Err(()) => Answered::Gone,
        };
    }

    // Last, right before the command runs: a token revoked while this
    // request waited is caught too.
    if !state.tokens.still_valid(credential) {
        return Answered::Close(revoked());
    }

    let reply = session.handle_text(text, hangup(gone.clone())).await;
    match send(sink, &reply).await {
        Ok(()) => Answered::Next,
        Err(()) => Answered::Gone,
    }
}

/// Wait (at most [`MAX_THROTTLE_WAIT`]) until the peer's rate admits one
/// request. A lockout or a saturated table is refused at once, and so is a
/// wait that would run past the bound. Only an admitted check spends a token,
/// so a request runs only on a token it actually holds.
async fn admit(state: &AppState, peer: SocketAddr) -> Result<(), Throttled> {
    let give_up = Instant::now() + MAX_THROTTLE_WAIT;
    loop {
        match state.throttle.check(peer.ip(), std::time::Instant::now()) {
            Ok(()) => return Ok(()),
            Err(Throttled::Rate(wait)) if Instant::now() + wait <= give_up => {
                tokio::time::sleep(wait).await;
            }
            Err(refused) => return Err(refused),
        }
    }
}

/// The answer to a request refused before it was parsed: an error response
/// to its id when one can be read, otherwise an error event.
fn refusal(text: &str, error: ProtoError) -> Message {
    #[derive(serde::Deserialize)]
    struct Id {
        id: Option<RequestId>,
    }
    match serde_json::from_str::<Id>(text) {
        Ok(Id { id: Some(id) }) => Message::err(id, error),
        _ => Message::event(Event::Error {
            session_id: None,
            error,
        }),
    }
}

/// How a bounded await ended.
enum Bounded<T> {
    Done(T),
    /// The server is shutting down.
    Closing,
    /// The deadline passed first.
    Expired,
}

/// Run `work` unless the server starts closing or `deadline` passes first; in
/// either case `work` is dropped where it stands.
async fn bounded<T>(
    closing: &mut watch::Receiver<bool>,
    deadline: Instant,
    work: impl Future<Output = T>,
) -> Bounded<T> {
    tokio::select! {
        biased;
        () = closed(closing) => Bounded::Closing,
        () = tokio::time::sleep_until(deadline) => Bounded::Expired,
        out = work => Bounded::Done(out),
    }
}

/// Resolves once the server is closing (or gone).
async fn closed(closing: &mut watch::Receiver<bool>) {
    let _ = closing.wait_for(|c| *c).await;
}

fn frame(code: u16, reason: &str) -> CloseFrame {
    CloseFrame {
        code,
        reason: reason.into(),
    }
}

fn server_closing() -> CloseFrame {
    frame(1001, "the server is shutting down")
}

fn revoked() -> CloseFrame {
    frame(
        1008,
        "the API token was revoked; reconnect with the current token",
    )
}

/// Resolves once the reader has seen the peer go.
fn hangup(mut gone: watch::Receiver<bool>) -> Hangup {
    Box::pin(async move {
        let _ = gone.wait_for(|g| *g).await;
    })
}

/// Write one envelope, giving up after [`SEND_TIMEOUT`].
async fn send(sink: &mut Sink, message: &Message) -> Result<(), ()> {
    let text = serde_json::to_string(message).map_err(|_| ())?;
    match tokio::time::timeout(SEND_TIMEOUT, sink.send(WsMessage::Text(text.into()))).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_request_is_answered_against_its_id_when_it_has_one() {
        let error = ProtoError::new(ErrorCode::RateLimited, "slow down");
        match refusal(
            r#"{"kind":"request","v":1,"id":7,"command":{"type":"get_status"}}"#,
            error.clone(),
        ) {
            Message::Response(r) => {
                assert_eq!(r.id, RequestId::Number(7));
                assert_eq!(
                    r.outcome.error().map(|e| e.code.clone()),
                    Some(ErrorCode::RateLimited)
                );
            }
            other => panic!("expected a response, got {other:?}"),
        }
        assert!(matches!(refusal("not json", error), Message::Event(_)));
    }
}
