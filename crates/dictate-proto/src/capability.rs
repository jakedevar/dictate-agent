//! The capability handshake.
//!
//! # Capabilities are per-connection, not per-server
//!
//! This is the load-bearing idea. One `dictated` process simultaneously serves
//! a local UDS socket and (via S33) a LAN listener, and the *same daemon* must
//! offer text injection on the first and refuse it on the second — a phone on
//! the wifi has no business synthesizing keystrokes into Jake's editor. A
//! headless daemon offers no UI affordances at all. So [`Capabilities`]
//! describes *this connection*, and a client must not cache it across
//! transports.
//!
//! # Everything defaults to unsupported
//!
//! Every flag in [`Features`] is `#[serde(default)]` over `bool`, so it
//! defaults to `false`. An older peer that has never heard of a capability
//! reads it as absent, which is the fail-safe direction: the failure mode of a
//! missing flag is "the client does not offer a button", never "the client
//! assumes it may inject text".

use serde::{Deserialize, Serialize};

use crate::state::Route;

/// Client → server opening message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    /// The protocol version the client would prefer to speak.
    pub protocol_version: u16,

    /// Every version the client can speak, so the server can pick an overlap
    /// instead of refusing outright. Defaults to just `protocol_version`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_versions: Vec<u16>,

    /// Who is connecting.
    pub client: ClientInfo,
}

impl Hello {
    /// Build a handshake for the current build's protocol version.
    #[must_use]
    pub fn new(client: ClientInfo) -> Self {
        Self {
            protocol_version: crate::PROTOCOL_VERSION,
            supported_versions: crate::SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
            client,
        }
    }

    /// Versions this client can speak, falling back to `protocol_version` when
    /// the list is omitted.
    #[must_use]
    pub fn versions(&self) -> Vec<u16> {
        if self.supported_versions.is_empty() {
            vec![self.protocol_version]
        } else {
            self.supported_versions.clone()
        }
    }
}

/// Identity of a connecting client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Free-form product name, e.g. `"dictate-cli"`, `"dictate-ui"`.
    pub name: String,
    /// Client build version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// What kind of client this is. The server may use it to pick a default
    /// capability set, but it is a *hint*: authorization is the server's
    /// decision, never the client's self-declaration.
    #[serde(default)]
    pub kind: ClientKind,
}

impl ClientInfo {
    /// Construct client identity.
    pub fn new(name: impl Into<String>, kind: ClientKind) -> Self {
        Self {
            name: name.into(),
            version: None,
            kind,
        }
    }
}

open_str_enum! {
    /// A hint about what sort of client is connecting.
    pub enum ClientKind {
        /// The local `dictate` command-line client.
        Cli => "cli",
        /// The Tauri desktop UI.
        DesktopUi => "desktop_ui",
        /// A remote client over the network API.
        Remote => "remote",
        /// An automated or scripted consumer.
        Automation => "automation",
    }
    default = Automation;
}

/// Server → client handshake response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerHello {
    /// The negotiated version. All subsequent messages on this connection use
    /// it.
    pub protocol_version: u16,
    /// Every version the server can speak.
    pub supported_versions: Vec<u16>,
    /// Who answered.
    pub server: ServerInfo,
    /// What this connection is permitted and able to do.
    pub capabilities: Capabilities,
}

/// Identity of the serving daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Product name, e.g. `"dictated"`.
    pub name: String,
    /// Daemon build version.
    pub version: String,
}

impl ServerInfo {
    /// Construct server identity.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}

/// What a given connection can do.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Feature flags.
    #[serde(default)]
    pub features: Features,

    /// Audio formats this connection will accept for upload or streaming.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_formats: Vec<crate::audio::AudioEncoding>,

    /// Routes this connection may invoke.
    ///
    /// Separate from [`Features`] because it is a *subset* decision, not an
    /// on/off one: a LAN connection may legitimately be allowed `type` output
    /// (returned as text) while being denied `timer`, which would run
    /// `systemd-run` on the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,

    /// Size and rate limits in force on this connection.
    #[serde(default)]
    pub limits: Limits,
}

impl Capabilities {
    /// Whether this connection may invoke the given route.
    ///
    /// Deny-by-default: an empty `routes` list permits *no* route. This is
    /// the fail-safe direction and matches [`Features`], where every flag
    /// defaults to `false` rather than to "on". A capability set that wants a
    /// route must name it explicitly — there is no implicit "unspecified
    /// means everything", because `Route::Timer` runs `systemd-run` on the
    /// host and `Route::Local` invokes the local LLM, and a peer that omits
    /// `routes` (an older client, a config bug, or a hand-rolled client) must
    /// not be silently granted either.
    #[must_use]
    pub fn allows_route(&self, route: &Route) -> bool {
        self.routes.contains(route)
    }

    /// The capability set for a trusted local connection: everything on.
    #[must_use]
    pub fn local_trusted() -> Self {
        Self {
            features: Features::local_trusted(),
            audio_formats: vec![
                crate::audio::AudioEncoding::PcmF32Le,
                crate::audio::AudioEncoding::PcmS16Le,
                crate::audio::AudioEncoding::Wav,
            ],
            routes: Route::known().to_vec(),
            limits: Limits::default(),
        }
    }

    /// The capability set for a remote client: transcription, no local effects.
    ///
    /// This is the shape the phone-client decision (Q3) implies — the client
    /// gets text back and the host's desktop, timers, and config stay
    /// untouched.
    #[must_use]
    pub fn remote_transcription_only() -> Self {
        Self {
            features: Features::remote_transcription_only(),
            audio_formats: vec![
                crate::audio::AudioEncoding::PcmF32Le,
                crate::audio::AudioEncoding::PcmS16Le,
                crate::audio::AudioEncoding::Wav,
            ],
            routes: vec![Route::Type],
            limits: Limits::default(),
        }
    }
}

/// Individual capability flags.
///
/// Every field is `bool` with `#[serde(default)]`. Adding a flag is an additive
/// change: older peers read the new field as `false`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Features {
    /// This connection may cause text to be injected into the host's focused
    /// application. **False for every remote connection.**
    #[serde(default)]
    pub text_injection: bool,

    /// The server emits [`Event::Partial`](crate::Event::Partial).
    ///
    /// Expected to be `false` for the foreseeable future: research spike R2
    /// returned DEFER on live partial transcripts. Clients must treat partials
    /// as an optional enhancement and render nothing when this is false.
    #[serde(default)]
    pub partial_transcripts: bool,

    /// The server emits [`Event::AudioLevel`](crate::Event::AudioLevel) for
    /// meters and HUD affordances.
    #[serde(default)]
    pub audio_level_events: bool,

    /// The connection accepts streamed binary audio frames (see
    /// [`crate::frame`]).
    #[serde(default)]
    pub streaming_audio: bool,

    /// The connection accepts a whole-clip upload via
    /// [`Command::TranscribeAudio`](crate::Command::TranscribeAudio).
    #[serde(default)]
    pub transcribe_upload: bool,

    /// The connection may drive a live capture session on the host's
    /// microphone. Distinct from `transcribe_upload`: a remote client may
    /// submit its own audio without being allowed to switch on the host's mic.
    #[serde(default)]
    pub host_capture: bool,

    /// History may be queried.
    #[serde(default)]
    pub history_read: bool,
    /// History may be purged by this trusted connection.
    #[serde(default)]
    pub history_write: bool,

    /// The personal dictionary may be read.
    #[serde(default)]
    pub dictionary_read: bool,
    /// The personal dictionary may be modified.
    #[serde(default)]
    pub dictionary_write: bool,

    /// Snippets may be read.
    #[serde(default)]
    pub snippets_read: bool,
    /// Snippets may be modified.
    #[serde(default)]
    pub snippets_write: bool,

    /// Configuration may be read.
    #[serde(default)]
    pub config_read: bool,
    /// Configuration may be modified.
    #[serde(default)]
    pub config_write: bool,

    /// The wake-word listener exists in this build (S34, conditional).
    #[serde(default)]
    pub wake_word: bool,

    /// The daemon is running without any desktop session attached, so no
    /// injection backend can ever become available on it.
    #[serde(default)]
    pub headless: bool,

    /// The daemon is in privacy mode: transcripts are not persisted, and
    /// history queries will return nothing for affected sessions.
    #[serde(default)]
    pub privacy_mode: bool,
}

impl Features {
    /// Everything a trusted local connection gets.
    #[must_use]
    pub fn local_trusted() -> Self {
        Self {
            text_injection: true,
            partial_transcripts: false,
            audio_level_events: true,
            streaming_audio: true,
            transcribe_upload: true,
            host_capture: true,
            history_read: true,
            history_write: true,
            dictionary_read: true,
            dictionary_write: true,
            snippets_read: true,
            snippets_write: true,
            config_read: true,
            config_write: true,
            wake_word: false,
            headless: false,
            privacy_mode: false,
        }
    }

    /// What a remote client gets: submit audio, receive text, touch nothing
    /// else.
    #[must_use]
    pub fn remote_transcription_only() -> Self {
        Self {
            transcribe_upload: true,
            streaming_audio: true,
            audio_level_events: true,
            ..Self::default()
        }
    }
}

/// Size and rate limits advertised to the client so it can fail fast locally
/// rather than by being disconnected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Largest accepted JSON message, in bytes.
    #[serde(default = "Limits::default_max_message_bytes")]
    pub max_message_bytes: u32,
    /// Largest accepted binary audio frame payload, in bytes.
    #[serde(default = "Limits::default_max_audio_frame_bytes")]
    pub max_audio_frame_bytes: u32,
    /// Longest accepted audio clip for a single upload, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_audio_ms: Option<u32>,
    /// Concurrent sessions this connection may run.
    #[serde(default = "Limits::default_max_concurrent_sessions")]
    pub max_concurrent_sessions: u16,
}

impl Limits {
    fn default_max_message_bytes() -> u32 {
        1024 * 1024
    }
    fn default_max_audio_frame_bytes() -> u32 {
        crate::frame::MAX_FRAME_PAYLOAD_BYTES
    }
    fn default_max_concurrent_sessions() -> u16 {
        1
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: Self::default_max_message_bytes(),
            max_audio_frame_bytes: Self::default_max_audio_frame_bytes(),
            max_audio_ms: None,
            max_concurrent_sessions: Self::default_max_concurrent_sessions(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scenario the handshake exists for: a phone on the LAN gets
    /// transcription but must never be able to type on Jake's desktop.
    #[test]
    fn remote_connection_cannot_inject_text() {
        let remote = Capabilities::remote_transcription_only();
        assert!(!remote.features.text_injection);
        assert!(remote.features.transcribe_upload);
        assert!(!remote.features.host_capture);
        assert!(!remote.features.config_write);
        assert!(!remote.features.dictionary_write);

        let local = Capabilities::local_trusted();
        assert!(local.features.text_injection);
        assert!(local.features.host_capture);
    }

    #[test]
    fn remote_connection_is_route_restricted() {
        let remote = Capabilities::remote_transcription_only();
        assert!(remote.allows_route(&Route::Type));
        // TIMER would run systemd-run on the host.
        assert!(!remote.allows_route(&Route::Timer));
        assert!(!remote.allows_route(&Route::Local));

        let local = Capabilities::local_trusted();
        for r in Route::known() {
            assert!(local.allows_route(r), "local must allow {r}");
        }
    }

    /// Fail-safe direction: an unknown or omitted flag reads as "not
    /// permitted", never as permitted.
    #[test]
    fn omitted_feature_flags_default_to_false() {
        let f: Features = serde_json::from_str("{}").unwrap();
        assert_eq!(f, Features::default());
        assert!(!f.text_injection);
        assert!(!f.config_write);

        // A newer daemon advertising a flag we have never heard of must not
        // disturb the flags we do understand.
        let f: Features =
            serde_json::from_str(r#"{"text_injection":true,"telepathy":true}"#).unwrap();
        assert!(f.text_injection);
        assert!(!f.host_capture);
    }

    #[test]
    fn partials_are_off_by_default_per_r2_defer() {
        assert!(!Features::default().partial_transcripts);
        assert!(!Features::local_trusted().partial_transcripts);
    }

    #[test]
    fn default_capabilities_permit_no_route() {
        let caps = Capabilities::default();
        for r in Route::known() {
            assert!(!caps.allows_route(r), "default must not allow {r}");
        }
    }

    #[test]
    fn capabilities_with_omitted_routes_field_permit_no_route() {
        // The v1-peer / hand-rolled-client case: `routes` is absent from the
        // wire payload entirely, not just empty.
        let caps: Capabilities = serde_json::from_str("{}").unwrap();
        for r in Route::known() {
            assert!(!caps.allows_route(r), "omitted routes must not allow {r}");
        }
    }

    #[test]
    fn hello_falls_back_to_single_version_when_list_omitted() {
        let h: Hello = serde_json::from_str(
            r#"{"protocol_version":1,"client":{"name":"phone","kind":"remote"}}"#,
        )
        .unwrap();
        assert_eq!(h.versions(), vec![1]);
        assert_eq!(h.client.kind, ClientKind::Remote);
        assert_eq!(h.client.version, None);
    }

    #[test]
    fn limits_defaults_apply_to_partial_json() {
        let l: Limits = serde_json::from_str("{}").unwrap();
        assert_eq!(l.max_message_bytes, 1024 * 1024);
        assert_eq!(l.max_concurrent_sessions, 1);
        assert_eq!(
            l.max_audio_frame_bytes,
            crate::frame::MAX_FRAME_PAYLOAD_BYTES
        );
    }

    #[test]
    fn hello_round_trips() {
        let h = Hello::new(ClientInfo::new("dictate-cli", ClientKind::Cli));
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<Hello>(&json).unwrap(), h);
    }
}
