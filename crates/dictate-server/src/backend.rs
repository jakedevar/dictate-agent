//! The seam between the transport and the daemon.
//!
//! The network API has no dispatcher of its own. Every command a network peer
//! sends is executed by whatever implements [`Backend`]; in `dictated` that is
//! a `NetworkConnection` over the unix socket's own `process`/`dispatch`, with
//! the network grant in place of the local one. So the parse rules, the
//! handshake, `is_permitted`, option resolution, upload decoding and session
//! ownership are the socket's, not a copy of them (design §3).
//!
//! The traits speak only `dictate-proto` types, which is why this crate does
//! not depend on the engine.

use std::future::Future;
use std::pin::Pin;

use dictate_proto::{Command, CommandResult, Event, Message, ProtoError};

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Resolves when the peer that asked for the request in flight has gone, so a
/// long request (an upload) can be cancelled rather than finished for nobody.
pub type Hangup = BoxFuture<'static, ()>;

/// A hang-up that never happens, for requests whose transport reports a
/// disconnect by dropping the request future instead (HTTP).
#[must_use]
pub fn never() -> Hangup {
    Box::pin(std::future::pending())
}

/// Opens connections to the daemon for network peers.
pub trait Backend: Send + Sync + 'static {
    /// A fresh connection holding the network grant and nothing else. It has
    /// not handshaken yet; until it does, every command but `handshake` is
    /// refused (`handshake_required`).
    fn open(&self) -> Box<dyn Session>;

    /// The largest envelope any connection this backend opens may accept at
    /// any point in its life (before or after its handshake). The WebSocket
    /// transport buffers no more than this per message; each message is then
    /// held to [`Session::max_message_bytes`].
    fn max_message_bytes(&self) -> usize;
}

/// One network peer's connection to the daemon.
///
/// Dropping it is the disconnect: whatever the connection owns (an upload in
/// flight) is cancelled, exactly as when a unix-socket peer hangs up.
pub trait Session: Send + 'static {
    /// One envelope as text, answered with the envelope to send back — the
    /// unix socket's line handling, including the `unsupported_command`
    /// answer for an unknown command and connection-level error events.
    fn handle_text<'a>(&'a mut self, text: &'a str, hangup: Hangup) -> BoxFuture<'a, Message>;

    /// The largest envelope this connection accepts now: the socket's
    /// pre-handshake ceiling until it handshakes, then its grant's
    /// `limits.max_message_bytes`. A longer message is refused unread
    /// (`payload_too_large`) and ends the connection, as on the socket.
    fn max_message_bytes(&self) -> usize;

    /// One typed command, for transports that build the command themselves
    /// (`POST /v1/transcribe`).
    fn execute(
        &mut self,
        command: Command,
        hangup: Hangup,
    ) -> BoxFuture<'_, Result<CommandResult, ProtoError>>;

    /// The next event this connection may see — already scoped to its own
    /// sessions and redacted to its capabilities. `None` when the daemon is
    /// shutting down. Cancel-safe.
    fn next_event(&mut self) -> BoxFuture<'_, Option<Event>>;
}
