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
//! # Bounds
//!
//! - Frame and message size: set on the upgrade from the connection's limits.
//! - Pipelining: at most [`PIPELINE`] requests queue behind the one in flight;
//!   one more closes the connection (1008) rather than buffering without end.
//! - Binary frames: refused (1003). Audio streaming is not implemented in this
//!   build (design §11); whole clips go as `transcribe_audio`.
//! - Idle: no client message for [`IDLE`] closes the session (1000).
//! - Rate: every message is charged to the peer's throttle bucket; an
//!   over-rate peer is slowed down, never answered out of order.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket};
use dictate_proto::Message;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info};

use crate::backend::Hangup;
use crate::http::AppState;
use crate::limit::Throttled;

/// Requests that may queue behind the one being answered.
pub(crate) const PIPELINE: usize = 4;
/// A session with no client message for this long is closed.
pub(crate) const IDLE: Duration = Duration::from_secs(300);
/// Longest a throttled message is held before it is answered.
const MAX_THROTTLE_WAIT: Duration = Duration::from_secs(5);

/// Why the reader stopped on its own.
#[derive(Debug, Clone, Copy)]
enum ReaderStop {
    /// The peer pipelined more than [`PIPELINE`] requests.
    Overloaded,
    /// The peer sent a binary frame.
    Binary,
}

pub(crate) async fn run(socket: WebSocket, state: Arc<AppState>, peer: SocketAddr) {
    info!(peer = %peer.ip(), "network API WebSocket session opened");
    let (mut sink, mut stream) = socket.split();
    let (queue, mut requests) = mpsc::channel::<String>(PIPELINE);
    let (gone_tx, gone) = watch::channel(false);

    let reader = tokio::spawn(async move {
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
    let idle = tokio::time::sleep(IDLE);
    tokio::pin!(idle);

    let close: Option<CloseFrame> = loop {
        tokio::select! {
            request = requests.recv() => {
                let Some(text) = request else { break None };
                if *gone.borrow() {
                    // Nobody is left to read the answer.
                    break None;
                }
                idle.as_mut().reset(tokio::time::Instant::now() + IDLE);
                if let Err(Throttled::Rate(wait) | Throttled::LockedOut(wait)) =
                    state.throttle.check(peer.ip(), std::time::Instant::now())
                {
                    tokio::time::sleep(wait.min(MAX_THROTTLE_WAIT)).await;
                }
                let reply = session.handle_text(&text, hangup(gone.clone())).await;
                if send(&mut sink, &reply).await.is_err() {
                    break None;
                }
            }
            event = session.next_event() => match event {
                Some(event) => {
                    if send(&mut sink, &Message::event(event)).await.is_err() {
                        break None;
                    }
                }
                None => break Some(frame(1001, "the daemon is shutting down")),
            },
            () = &mut idle => break Some(frame(1000, "idle")),
            _ = closing.changed() => break Some(frame(1001, "the server is shutting down")),
        }
    };

    let close = match close {
        Some(frame) => {
            reader.abort();
            Some(frame)
        }
        None => match reader.await.ok().flatten() {
            Some(ReaderStop::Overloaded) => Some(frame(1008, "too many pipelined requests")),
            Some(ReaderStop::Binary) => Some(frame(
                1003,
                "binary audio frames are not supported by this server; send transcribe_audio",
            )),
            None => None,
        },
    };
    if let Some(frame) = close {
        debug!(peer = %peer.ip(), code = frame.code, reason = %frame.reason, "closing WebSocket");
        let _ = sink.send(WsMessage::Close(Some(frame))).await;
    }
    let _ = sink.close().await;
    // Dropping the daemon connection is the disconnect: an upload this
    // session still owns is cancelled.
    drop(session);
    info!(peer = %peer.ip(), "network API WebSocket session closed");
}

fn frame(code: u16, reason: &str) -> CloseFrame {
    CloseFrame {
        code,
        reason: reason.into(),
    }
}

/// Resolves once the reader has seen the peer go.
fn hangup(mut gone: watch::Receiver<bool>) -> Hangup {
    Box::pin(async move {
        let _ = gone.wait_for(|g| *g).await;
    })
}

async fn send(
    sink: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    message: &Message,
) -> Result<(), ()> {
    let text = serde_json::to_string(message).map_err(|_| ())?;
    sink.send(WsMessage::Text(text.into()))
        .await
        .map_err(|_| ())
}
