//! The versioned envelope wrapping everything on a streaming connection.
//!
//! # Shape
//!
//! ```json
//! {"kind":"request", "v":1, "id":1, "command":{"type":"stop"}}
//! {"kind":"response","v":1, "id":1, "result":{"type":"ack"}}
//! {"kind":"response","v":1, "id":1, "error":{"code":"busy","message":"..."}}
//! {"kind":"event",   "v":1, "event":{"type":"state_changed","from":"idle","to":"recording",...}}
//! ```
//!
//! `kind` discriminates direction, `v` pins the protocol version on every
//! message rather than only at handshake time, and `id` correlates a response
//! with its request. Events carry no `id` because they are unsolicited.
//!
//! # The envelope is optional
//!
//! [`Command`], [`CommandResult`], and [`Event`] are all usable standalone, and
//! that is deliberate. `POST /v1/transcribe` should take a bare command body
//! and return a bare result — wrapping a stateless HTTP request in a
//! correlation id it does not need would be ceremony, and would make the
//! endpoint harder to call with `curl`. The envelope earns its keep only on a
//! multiplexed connection.
//!
//! # Framing
//!
//! On a WebSocket, one text message is one [`Message`]. On the daemon's unix
//! socket, messages are newline-delimited JSON — safe because `serde_json`
//! escapes newlines inside strings, so a raw `\n` can only ever be a delimiter.
//! See [`Message::to_ndjson_line`].

use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::error::{ErrorCode, ProtoError};
use crate::event::Event;
use crate::result::CommandResult;

/// Correlates a response with its request.
///
/// Accepts a JSON number or a string, matching JSON-RPC 2.0 practice, so a
/// client may use whichever its language makes natural.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// A numeric identifier.
    Number(u64),
    /// A string identifier.
    Text(String),
}

impl From<u64> for RequestId {
    fn from(v: u64) -> Self {
        Self::Number(v)
    }
}

impl From<String> for RequestId {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}

impl From<&str> for RequestId {
    fn from(v: &str) -> Self {
        Self::Text(v.to_string())
    }
}

impl core::fmt::Display for RequestId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Text(s) => f.write_str(s),
        }
    }
}

/// A client's request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Protocol version.
    #[serde(default = "default_version")]
    pub v: u16,
    /// Correlation id, echoed in the response.
    pub id: RequestId,
    /// What to do.
    pub command: Command,
}

/// A server's answer to exactly one [`Request`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Protocol version.
    #[serde(default = "default_version")]
    pub v: u16,
    /// The correlation id from the request.
    pub id: RequestId,
    /// Success or failure. Flattened, so exactly one of `result` or `error`
    /// appears at the top level — the type makes "both" and "neither"
    /// unrepresentable.
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// Success or failure of a command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The command succeeded.
    Result(CommandResult),
    /// The command failed.
    Error(ProtoError),
}

impl Outcome {
    /// Whether the command succeeded.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Result(_))
    }

    /// Borrow the success value.
    #[must_use]
    pub fn result(&self) -> Option<&CommandResult> {
        match self {
            Self::Result(r) => Some(r),
            Self::Error(_) => None,
        }
    }

    /// Borrow the failure.
    #[must_use]
    pub fn error(&self) -> Option<&ProtoError> {
        match self {
            Self::Error(e) => Some(e),
            Self::Result(_) => None,
        }
    }
}

impl From<Outcome> for Result<CommandResult, ProtoError> {
    fn from(o: Outcome) -> Self {
        match o {
            Outcome::Result(r) => Ok(r),
            Outcome::Error(e) => Err(e),
        }
    }
}

/// An unsolicited server-to-client event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Protocol version.
    #[serde(default = "default_version")]
    pub v: u16,
    /// The event.
    pub event: Event,
}

fn default_version() -> u16 {
    crate::PROTOCOL_VERSION
}

/// Anything that can appear on a streaming connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    /// Client to server.
    Request(Request),
    /// Server to client, answering a request.
    Response(Response),
    /// Server to client, unsolicited.
    Event(EventEnvelope),
}

impl Message {
    /// Build a request.
    pub fn request(id: impl Into<RequestId>, command: Command) -> Self {
        Self::Request(Request {
            v: crate::PROTOCOL_VERSION,
            id: id.into(),
            command,
        })
    }

    /// Build a successful response.
    pub fn ok(id: impl Into<RequestId>, result: CommandResult) -> Self {
        Self::Response(Response {
            v: crate::PROTOCOL_VERSION,
            id: id.into(),
            outcome: Outcome::Result(result),
        })
    }

    /// Build a failure response.
    pub fn err(id: impl Into<RequestId>, error: ProtoError) -> Self {
        Self::Response(Response {
            v: crate::PROTOCOL_VERSION,
            id: id.into(),
            outcome: Outcome::Error(error),
        })
    }

    /// Build an event.
    #[must_use]
    pub fn event(event: Event) -> Self {
        Self::Event(EventEnvelope {
            v: crate::PROTOCOL_VERSION,
            event,
        })
    }

    /// The protocol version this message declares.
    #[must_use]
    pub fn version(&self) -> u16 {
        match self {
            Self::Request(r) => r.v,
            Self::Response(r) => r.v,
            Self::Event(e) => e.v,
        }
    }

    /// The correlation id, for requests and responses.
    #[must_use]
    pub fn id(&self) -> Option<&RequestId> {
        match self {
            Self::Request(r) => Some(&r.id),
            Self::Response(r) => Some(&r.id),
            Self::Event(_) => None,
        }
    }

    /// Serialize to a single JSON line terminated by `\n`.
    ///
    /// The framing for the daemon's unix socket. Safe because `serde_json`
    /// escapes newlines inside strings, so the only literal `\n` in the output
    /// is the terminator.
    pub fn to_ndjson_line(&self) -> Result<String, serde_json::Error> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }

    /// Parse a message, converting a parse failure into the error the peer
    /// should be sent.
    ///
    /// The distinction this encodes: a *version* mismatch is reported as
    /// [`ErrorCode::UnsupportedVersion`], and anything else as
    /// [`ErrorCode::MalformedRequest`] — including an unknown command, which by
    /// the compatibility rule must fail rather than be ignored. Callers that
    /// can recover the request id should upgrade the latter to
    /// [`ProtoError::unsupported_command`] with the actual name.
    pub fn parse(input: &str) -> Result<Self, ProtoError> {
        let msg: Self = serde_json::from_str(input)
            .map_err(|e| ProtoError::new(ErrorCode::MalformedRequest, e.to_string()))?;
        if !crate::supports_version(msg.version()) {
            return Err(ProtoError::new(
                ErrorCode::UnsupportedVersion,
                format!(
                    "protocol version {} is not supported; this build speaks {:?}",
                    msg.version(),
                    crate::SUPPORTED_PROTOCOL_VERSIONS
                ),
            ));
        }
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    #[test]
    fn request_wire_shape() {
        let m = Message::request(1u64, Command::Stop);
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"kind":"request","v":1,"id":1,"command":{"type":"stop"}}"#
        );
        assert_eq!(m.version(), crate::PROTOCOL_VERSION);
        assert_eq!(m.id(), Some(&RequestId::Number(1)));
    }

    #[test]
    fn success_response_wire_shape() {
        let m = Message::ok(1u64, CommandResult::Ack);
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"kind":"response","v":1,"id":1,"result":{"type":"ack"}}"#
        );
    }

    #[test]
    fn error_response_wire_shape() {
        let m = Message::err(
            "abc",
            ProtoError::new(ErrorCode::Busy, "a session is already running"),
        );
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"kind":"response","v":1,"id":"abc","error":{"code":"busy","message":"a session is already running"}}"#
        );
    }

    #[test]
    fn event_wire_shape_has_no_id() {
        let m = Message::event(Event::StateChanged {
            session_id: "s1".into(),
            from: State::Idle,
            to: State::Recording,
            at_ms: None,
        });
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["kind"], "event");
        assert!(v.get("id").is_none(), "events are unsolicited");
        assert_eq!(v["event"]["type"], "state_changed");
        assert!(m.id().is_none());
    }

    #[test]
    fn every_message_kind_round_trips() {
        let msgs = vec![
            Message::request(7u64, Command::GetStatus),
            Message::request("uuid-like", Command::Cancel),
            Message::ok(7u64, CommandResult::Ack),
            Message::err(7u64, ProtoError::new(ErrorCode::NotFound, "nope")),
            Message::event(Event::Unknown),
        ];
        for m in msgs {
            let line = m.to_ndjson_line().unwrap();
            assert!(line.ends_with('\n'));
            assert_eq!(line.matches('\n').count(), 1);
            assert_eq!(Message::parse(line.trim()).unwrap(), m);
        }
    }

    #[test]
    fn request_ids_accept_numbers_and_strings() {
        let n: Message = serde_json::from_str(
            r#"{"kind":"request","v":1,"id":42,"command":{"type":"stop"}}"#,
        )
        .unwrap();
        assert_eq!(n.id(), Some(&RequestId::Number(42)));

        let s: Message = serde_json::from_str(
            r#"{"kind":"request","v":1,"id":"42","command":{"type":"stop"}}"#,
        )
        .unwrap();
        assert_eq!(s.id(), Some(&RequestId::Text("42".into())));
        assert_ne!(n.id(), s.id(), "42 and \"42\" are different ids");
    }

    #[test]
    fn outcome_is_exactly_one_of_result_or_error() {
        let ok = Outcome::Result(CommandResult::Ack);
        assert!(ok.is_ok());
        assert!(ok.result().is_some());
        assert!(ok.error().is_none());

        let err = Outcome::Error(ProtoError::new(ErrorCode::Internal, "x"));
        assert!(!err.is_ok());
        assert!(err.error().is_some());
        assert!(err.result().is_none());

        let as_result: Result<CommandResult, ProtoError> = err.into();
        assert!(as_result.is_err());
    }

    #[test]
    fn newlines_in_payloads_cannot_break_ndjson_framing() {
        let m = Message::err(
            1u64,
            ProtoError::new(ErrorCode::Internal, "line one\nline two\r\nline three"),
        );
        let line = m.to_ndjson_line().unwrap();
        assert_eq!(
            line.matches('\n').count(),
            1,
            "embedded newlines must be escaped, not emitted raw: {line}"
        );
        assert_eq!(Message::parse(line.trim()).unwrap(), m);
    }

    #[test]
    fn a_future_protocol_version_is_refused_with_a_specific_code() {
        let e = Message::parse(r#"{"kind":"request","v":99,"id":1,"command":{"type":"stop"}}"#)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::UnsupportedVersion);
        assert!(e.message.contains("99"));
    }

    /// An unknown command must surface as an error, never be dropped.
    #[test]
    fn an_unknown_command_is_a_parse_error_not_a_silent_drop() {
        let e = Message::parse(
            r#"{"kind":"request","v":1,"id":1,"command":{"type":"summon_kraken"}}"#,
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::MalformedRequest);
    }

    #[test]
    fn version_defaults_to_current_when_omitted() {
        let m: Message =
            serde_json::from_str(r#"{"kind":"request","id":1,"command":{"type":"stop"}}"#).unwrap();
        assert_eq!(m.version(), crate::PROTOCOL_VERSION);
    }

    #[test]
    fn unknown_envelope_kind_is_rejected() {
        assert!(Message::parse(r#"{"kind":"telepathy","v":1}"#).is_err());
    }

    #[test]
    fn garbage_input_yields_malformed_request() {
        let e = Message::parse("not json at all").unwrap_err();
        assert_eq!(e.code, ErrorCode::MalformedRequest);
    }
}
