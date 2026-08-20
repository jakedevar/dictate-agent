//! # `dictate-proto` — the one wire contract for dictate-agent
//!
//! This crate defines **types and (de)serialization only**. There is no
//! transport, no async runtime, and no I/O here. Three separately-built
//! consumers share it:
//!
//! | Consumer | Slice | Transport |
//! |---|---|---|
//! | `dictated` local control plane | S02 | JSON-RPC over a unix domain socket |
//! | `dictate-server` network API | S33 | WebSocket (control + binary audio) and `POST /v1/transcribe` |
//! | Tauri v2 UI | S32 | the daemon's UDS socket, via the Tauri bridge |
//!
//! A fourth consumer is anticipated but out of scope: a thin phone client that
//! streams audio and takes formatted text back (Decision Q3, 2026-08-07).
//!
//! ## Compatibility rule (the contract this crate exists to enforce)
//!
//! The wire format is versioned by [`PROTOCOL_VERSION`], a single integer
//! carried in every [`Message`]. Within one `PROTOCOL_VERSION` the format is
//! **additive-only**:
//!
//! **MAY be added** in a minor change (no version bump):
//! - a new optional field, if it is `#[serde(default)]` and its default
//!   preserves the old behavior;
//! - a new variant of an *open* enum (see below);
//! - a new [`Command`], [`Event`], [`CommandResult`], or [`ErrorCode`];
//! - a new capability flag in [`Features`] (defaults to `false` = unsupported).
//!
//! **MUST NOT change** without incrementing [`PROTOCOL_VERSION`]:
//! - renaming or removing any field or variant;
//! - changing a field's type, its units, or its meaning;
//! - making an optional field required, or removing a `default`;
//! - changing an enum's serde tagging, or a struct's nesting;
//! - changing the binary frame layout in [`frame`] (it has its own
//!   [`frame::FRAME_VERSION`], bumped independently).
//!
//! These rules are mechanically enforced by the golden-JSON tests in
//! `tests/golden.rs`. A change that violates them fails CI with a diff of the
//! exact wire bytes, which is the point: breaking the contract must be loud
//! and deliberate, never accidental.
//!
//! ### Open vs. closed enums, and the asymmetry that matters
//!
//! *Open* enums tolerate values a peer has never heard of. They round-trip
//! unknown values **losslessly** (via `#[serde(from/into = "String")]`), so a
//! proxy or a logger never silently corrupts traffic it does not understand:
//! [`State`], [`Route`], [`DictationMode`], [`ErrorCode`], [`SkipReason`],
//! [`InjectMethod`].
//!
//! [`Event`] and [`CommandResult`] are open in a weaker sense: an unrecognized
//! variant deserializes to `Unknown` and the payload is dropped. That is safe
//! for a *terminal* consumer (you cannot act on an event you do not
//! understand) but a **relay must forward raw bytes rather than re-serializing
//! a parsed value**, or it will flatten a newer peer's events into `Unknown`.
//!
//! [`Command`] is deliberately **closed**. An unknown command fails to
//! deserialize, and the server is required to answer
//! [`ErrorCode::UnsupportedCommand`]. The asymmetry is the design:
//!
//! > Unknown on the **event/result** path degrades silently — a client must
//! > survive a newer daemon. Unknown on the **command** path errors loudly — a
//! > server must never silently drop a request it cannot honor, because the
//! > caller would otherwise wait forever for an effect that never happens.
//!
//! Unknown *fields* are ignored everywhere: no type in this crate uses
//! `deny_unknown_fields`.
//!
//! ## Capability handshake
//!
//! Capabilities are negotiated **per connection, not per server** — the same
//! daemon offers text injection over its local UDS socket but must not offer
//! it to a phone on the LAN, and a headless daemon has no UI at all. See
//! [`Capabilities`] and [`Features`]. Every [`Features`] flag defaults to
//! `false`, so an older peer that omits a flag is read as "not supported",
//! which is the fail-safe direction.
//!
//! ## Dependency budget
//!
//! `serde`, `serde_json`, `base64`. Nothing else, and nothing async. This crate
//! must stay cheap enough for a Tauri UI and a thin phone client to depend on.
//! The `base64` justification is recorded in `Cargo.toml`. Notably absent:
//! `thiserror` (the `Display`/`Error` impls here are hand-written to hold the
//! line), `chrono` and `uuid` (timestamps are `u64` milliseconds since the Unix
//! epoch and identifiers are opaque strings, so no peer is forced to adopt our
//! date or UUID library).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[macro_use]
mod macros;

pub mod audio;
pub mod capability;
pub mod command;
pub mod envelope;
pub mod error;
pub mod event;
pub mod frame;
pub mod records;
pub mod result;
pub mod state;
pub mod timings;

pub use audio::{AudioEncoding, AudioFormat, AudioSource};
pub use capability::{
    Capabilities, ClientInfo, ClientKind, Features, Hello, Limits, ServerHello, ServerInfo,
};
pub use command::{Command, SessionOptions};
pub use envelope::{EventEnvelope, Message, Outcome, RequestId, Request, Response};
pub use error::{ErrorCode, ProtoError};
pub use event::{Event, FinalText, Hypothesis};
pub use frame::{AudioFrame, FrameError, FrameKind};
pub use records::{
    ConfigEntry, ConfigSnapshot, DailyWords, DictionaryEntry, EntrySource, HistoryAnalytics,
    HistoryEntry, HistoryPage, HistoryQuery, SortOrder, Snippet,
};
pub use result::{CommandResult, DaemonInfo, ModelStatus, SessionSummary, Status, Transcript};
pub use state::{DictationMode, InjectMethod, InjectionOutcome, Route, SessionId, State};
pub use timings::{SkipReason, StageTiming, StageTimings};

/// The protocol major version carried in every [`Message`].
///
/// Incremented **only** for a change forbidden by the compatibility rule in
/// the crate docs. Additive changes do not touch it.
pub const PROTOCOL_VERSION: u16 = 1;

/// Protocol versions this build can speak, newest first.
///
/// A server advertises this in [`ServerHello::supported_versions`] so a client
/// built against an older version can pick a version both sides understand
/// instead of failing the connection outright.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[u16] = &[1];

/// Whether this build can speak the given protocol version.
#[must_use]
pub fn supports_version(v: u16) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&v)
}

/// Negotiate the highest protocol version both peers support.
///
/// Returns `None` when there is no overlap, which the server must report as
/// [`ErrorCode::UnsupportedVersion`] rather than closing the connection
/// silently — an older client deserves to be told why it was refused.
#[must_use]
pub fn negotiate_version(peer_supported: &[u16]) -> Option<u16> {
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .filter(|v| peer_supported.contains(v))
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_highest_common_version() {
        assert_eq!(negotiate_version(&[1]), Some(1));
        assert_eq!(negotiate_version(&[1, 2]), Some(1));
        assert_eq!(negotiate_version(&[2, 3]), None);
        assert_eq!(negotiate_version(&[]), None);
    }

    #[test]
    fn supports_current_version() {
        assert!(supports_version(PROTOCOL_VERSION));
        assert!(!supports_version(PROTOCOL_VERSION + 1));
    }
}
