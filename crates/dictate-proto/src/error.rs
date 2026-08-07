//! The error type carried by [`Response`](crate::Response) and
//! [`Event::Error`](crate::Event::Error).

use serde::{Deserialize, Serialize};

open_str_enum! {
    /// A stable, machine-readable error classification.
    ///
    /// Codes are strings rather than integers so that a new code added by a
    /// newer daemon is self-describing in a log rather than an unexplained
    /// number, and so that adding one is an additive change.
    ///
    /// The authentication and authorization codes are modeled here but **not
    /// implemented** by this crate. S33 owns bearer-token auth; the protocol's
    /// job is only to ensure it has somewhere well-defined to put the answer.
    pub enum ErrorCode {
        // -- Protocol ---------------------------------------------------
        /// The peer's protocol version has no overlap with ours.
        UnsupportedVersion => "unsupported_version",
        /// The command is not one this build knows. A server MUST answer with
        /// this rather than ignoring a command it cannot parse.
        UnsupportedCommand => "unsupported_command",
        /// The frame or JSON payload could not be parsed.
        MalformedRequest => "malformed_request",
        /// The command parsed but its arguments are invalid.
        InvalidParams => "invalid_params",
        /// A handshake is required before this command is accepted.
        HandshakeRequired => "handshake_required",

        // -- Authentication / authorization (modeled for S33) -----------
        /// No credential, or an invalid one.
        Unauthorized => "unauthorized",
        /// The credential is valid but this connection's negotiated
        /// capabilities do not include the requested operation — a LAN client
        /// asking to inject text into the host's desktop, for example.
        Forbidden => "forbidden",
        /// The client exceeded a rate limit.
        RateLimited => "rate_limited",
        /// The request body exceeded the server's configured limit.
        PayloadTooLarge => "payload_too_large",

        // -- Capability / consent (R3) ----------------------------------
        /// The operation is not available on this backend, platform, or
        /// connection. Not a malfunction.
        CapabilityUnavailable => "capability_unavailable",
        /// The operation needs user consent that has not been granted yet.
        ConsentRequired => "consent_required",
        /// The user was asked for consent and refused.
        ConsentDenied => "consent_denied",

        // -- Session state ----------------------------------------------
        /// A session is already in flight and this daemon serves one at a time.
        Busy => "busy",
        /// The command is not legal in the session's current state.
        InvalidState => "invalid_state",
        /// The command needs a session and none is running.
        NoActiveSession => "no_active_session",
        /// The operation was cancelled before it completed.
        Cancelled => "cancelled",
        /// The operation exceeded its deadline.
        Timeout => "timeout",

        // -- Pipeline ----------------------------------------------------
        /// The capture device is missing, busy, or failed mid-session.
        AudioDeviceError => "audio_device_error",
        /// The submitted audio could not be decoded, or its format is
        /// unsupported.
        AudioFormatUnsupported => "audio_format_unsupported",
        /// Speech-to-text inference failed.
        SttFailed => "stt_failed",
        /// The requested model is not downloaded or could not be loaded.
        ModelUnavailable => "model_unavailable",
        /// The formatting stage failed in a way that could not fail open.
        FormattingFailed => "formatting_failed",
        /// Injection failed. Distinct from consent and capability errors.
        InjectionFailed => "injection_failed",
        /// A history or database operation failed.
        HistoryError => "history_error",
        /// The submitted configuration is invalid and was not applied.
        ConfigInvalid => "config_invalid",
        /// The addressed record does not exist.
        NotFound => "not_found",
        /// The record already exists and the command refused to overwrite it.
        Conflict => "conflict",

        /// An unclassified server-side failure.
        Internal => "internal",
    }
}

impl ErrorCode {
    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// Advisory: it tells a client whether a retry is *sane*, not whether it is
    /// *permitted*. Honor [`ProtoError::retry_after_ms`] when it is present.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Busy
                | Self::RateLimited
                | Self::Timeout
                | Self::Internal
                | Self::AudioDeviceError
                | Self::ModelUnavailable
        )
    }

    /// Whether this error reflects a missing capability rather than a fault.
    ///
    /// A client should present these as "not available here", not as an error
    /// the user did something wrong to cause.
    #[must_use]
    pub fn is_capability_gap(&self) -> bool {
        matches!(
            self,
            Self::CapabilityUnavailable | Self::UnsupportedCommand | Self::Forbidden
        )
    }

    /// The HTTP status S33 should map this code to.
    ///
    /// Lives here rather than in `dictate-server` so that the WebSocket path
    /// and the `POST /v1/transcribe` path cannot drift apart, and so the
    /// mapping is testable without standing up a server.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::MalformedRequest
            | Self::InvalidParams
            | Self::AudioFormatUnsupported
            | Self::ConfigInvalid
            | Self::HandshakeRequired => 400,
            Self::Unauthorized => 401,
            Self::Forbidden | Self::ConsentDenied => 403,
            Self::NotFound | Self::NoActiveSession => 404,
            Self::Conflict | Self::InvalidState | Self::Busy => 409,
            Self::PayloadTooLarge => 413,
            Self::RateLimited => 429,
            Self::Cancelled => 499,
            Self::Timeout => 504,
            Self::UnsupportedVersion
            | Self::UnsupportedCommand
            | Self::CapabilityUnavailable
            | Self::ConsentRequired => 501,
            Self::ModelUnavailable | Self::AudioDeviceError => 503,
            Self::SttFailed
            | Self::FormattingFailed
            | Self::InjectionFailed
            | Self::HistoryError
            | Self::Internal
            | Self::Unknown(_) => 500,
        }
    }
}

/// A protocol-level error.
///
/// `Display` and `std::error::Error` are hand-written rather than derived via
/// `thiserror`, to keep this crate's dependency budget at three (see the
/// crate-level "Dependency budget").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtoError {
    /// Machine-readable classification.
    pub code: ErrorCode,

    /// Human-readable summary. Must not contain transcript text or other user
    /// content when the daemon is in privacy mode.
    pub message: String,

    /// Structured, code-specific detail. Free-form so that adding context to an
    /// existing code stays an additive change.
    ///
    /// Boxed because it is almost always absent, and this type is returned by
    /// value from `Result` throughout S02 and S33 — an inline
    /// `serde_json::Value` would make every error in the codebase 56 bytes
    /// larger for a field nobody usually sets. Invisible on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Box<serde_json::Value>>,

    /// How long the client should wait before retrying, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl ProtoError {
    /// Construct an error with a code and message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
            retry_after_ms: None,
        }
    }

    /// Attach structured detail.
    #[must_use]
    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(Box::new(detail));
        self
    }

    /// Borrow the structured detail, if any.
    #[must_use]
    pub fn detail(&self) -> Option<&serde_json::Value> {
        self.detail.as_deref()
    }

    /// Attach a retry hint.
    #[must_use]
    pub fn with_retry_after_ms(mut self, ms: u64) -> Self {
        self.retry_after_ms = Some(ms);
        self
    }

    /// Whether retrying could plausibly succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.code.is_retryable()
    }

    /// Shorthand for [`ErrorCode::UnsupportedCommand`], the required answer to
    /// a command the server cannot parse.
    pub fn unsupported_command(name: impl Into<String>) -> Self {
        let name = name.into();
        Self::new(
            ErrorCode::UnsupportedCommand,
            format!("unsupported command: {name}"),
        )
    }

    /// Shorthand for [`ErrorCode::CapabilityUnavailable`].
    pub fn capability_unavailable(what: impl Into<String>) -> Self {
        let what = what.into();
        Self::new(
            ErrorCode::CapabilityUnavailable,
            format!("capability unavailable: {what}"),
        )
    }
}

impl core::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ProtoError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_code_and_message() {
        let e = ProtoError::new(ErrorCode::Busy, "a session is already running");
        assert_eq!(e.to_string(), "[busy] a session is already running");
    }

    #[test]
    fn builders_attach_optional_fields() {
        let e = ProtoError::new(ErrorCode::RateLimited, "slow down")
            .with_retry_after_ms(2500)
            .with_detail(serde_json::json!({"limit": 10}));
        assert_eq!(e.retry_after_ms, Some(2500));
        assert_eq!(e.detail(), Some(&serde_json::json!({"limit": 10})));
        assert!(e.is_retryable());
        // Boxing detail must not change the wire form.
        assert_eq!(
            serde_json::to_value(&e).unwrap()["detail"],
            serde_json::json!({"limit": 10})
        );
    }

    #[test]
    fn optional_fields_are_omitted_when_absent() {
        let e = ProtoError::new(ErrorCode::Internal, "boom");
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"code":"internal","message":"boom"}"#);
    }

    #[test]
    fn consent_and_capability_codes_exist_and_map_to_http() {
        // R3 forward-compat: these must be expressible without a version bump.
        assert_eq!(ErrorCode::ConsentRequired.http_status(), 501);
        assert_eq!(ErrorCode::ConsentDenied.http_status(), 403);
        assert_eq!(ErrorCode::CapabilityUnavailable.http_status(), 501);
        assert!(ErrorCode::CapabilityUnavailable.is_capability_gap());
        assert!(!ErrorCode::ConsentDenied.is_capability_gap());
    }

    #[test]
    fn authz_codes_map_to_distinct_http_statuses() {
        assert_eq!(ErrorCode::Unauthorized.http_status(), 401);
        assert_eq!(ErrorCode::Forbidden.http_status(), 403);
        assert_eq!(ErrorCode::RateLimited.http_status(), 429);
        assert_eq!(ErrorCode::PayloadTooLarge.http_status(), 413);
    }

    #[test]
    fn unknown_code_round_trips_and_is_a_server_error() {
        let e: ProtoError =
            serde_json::from_str(r#"{"code":"flux_capacitor_drained","message":"?"}"#).unwrap();
        assert_eq!(
            e.code,
            ErrorCode::Unknown("flux_capacitor_drained".into())
        );
        assert!(!e.code.is_known());
        assert_eq!(e.code.http_status(), 500);
        assert!(!e.is_retryable());
        assert!(serde_json::to_string(&e)
            .unwrap()
            .contains("flux_capacitor_drained"));
    }

    #[test]
    fn every_known_code_has_a_plausible_http_status() {
        for code in ErrorCode::known() {
            let s = code.http_status();
            assert!((400..=599).contains(&s), "{code} -> {s}");
        }
    }

    #[test]
    fn implements_std_error() {
        fn assert_error<E: std::error::Error>(_: &E) {}
        assert_error(&ProtoError::new(ErrorCode::Internal, "x"));
    }
}
