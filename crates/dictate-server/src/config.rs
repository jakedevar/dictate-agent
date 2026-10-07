//! `[api]`: whether and where the network API listens, and what a network peer
//! may do.
//!
//! Everything here is a pure function of the configuration, so the security
//! decisions — *may this daemon bind that address?* and *what does a network
//! peer get?* — are unit-tested exhaustively rather than inferred from a
//! running server. See `thoughts/shared/plans/2026-10-07-s33-network-api-security-design.md`
//! §4 and §7.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use dictate_proto::{Capabilities, Limits, Route};
use serde::{Deserialize, Serialize};

/// The port the API listens on when `[api] bind` is not set.
pub const DEFAULT_PORT: u16 = 7313;

/// The file holding the bearer token, under this daemon's data directory.
const TOKEN_FILE: &str = "api-token";
/// This daemon's data directory name (shared with `dictated`'s history path;
/// deliberately not the Python daemon's `dictate-agent`).
const DATA_DIR: &str = "dictated";

/// `[api]` configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiConfig {
    /// Start the network API at all. Off by default: a default install opens
    /// no TCP port.
    pub enabled: bool,
    /// `IP:port` to listen on. Loopback unless `allow_lan`. A hostname is
    /// refused (it could resolve to anything).
    pub bind: String,
    /// Permit a non-loopback `bind`. Required, together with TLS or
    /// `allow_plaintext_lan`, for anything that leaves this host.
    pub allow_lan: bool,
    /// Acknowledge that a non-loopback `bind` is an encrypted tunnel interface
    /// (WireGuard, Tailscale), so plaintext HTTP on it is acceptable. Never set
    /// this for a Wi-Fi or Ethernet address: the bearer token would cross the
    /// network in the clear.
    pub allow_plaintext_lan: bool,
    /// PEM certificate chain. With `tls_key`, serve HTTPS.
    pub tls_cert: String,
    /// PEM private key for `tls_cert`.
    pub tls_key: String,
    /// Where the bearer token lives. Empty means
    /// `$XDG_DATA_HOME/dictated/api-token`. Create it with
    /// `dictated --api-token`; the daemon never creates one on its own.
    pub token_file: String,
    /// Extra `Host` header values to accept (e.g. a `*.ts.net` name when
    /// `tailscale serve` fronts a loopback bind). The bound address's own
    /// forms are always accepted.
    pub allowed_hosts: Vec<String>,
    /// `Origin` header values to accept. Empty (the default) refuses every
    /// request that carries an `Origin` — i.e. every browser.
    pub allowed_origins: Vec<String>,
    /// Let network peers see `raw_text` (recognizer output before formatting).
    pub expose_raw_text: bool,
    /// Largest accepted upload body, in bytes.
    pub max_upload_bytes: u64,
    /// Longest accepted clip, in seconds of audio.
    pub max_audio_seconds: u32,
    /// Concurrent TCP connections; further connections are closed at accept.
    pub max_connections: u32,
    /// Concurrent WebSocket sessions.
    pub max_ws_sessions: u32,
    /// Sustained requests per minute per peer IP (WebSocket messages count).
    pub requests_per_minute: u32,
    /// Requests a peer IP may make in a burst before `requests_per_minute`
    /// applies.
    pub burst: u32,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: format!("127.0.0.1:{DEFAULT_PORT}"),
            allow_lan: false,
            allow_plaintext_lan: false,
            tls_cert: String::new(),
            tls_key: String::new(),
            token_file: String::new(),
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            expose_raw_text: false,
            max_upload_bytes: 32 * 1024 * 1024,
            max_audio_seconds: 300,
            max_connections: 16,
            max_ws_sessions: 4,
            requests_per_minute: 120,
            burst: 20,
        }
    }
}

/// How bytes travel on the bound socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// Plain HTTP.
    Plain,
    /// HTTPS with this certificate chain and key.
    Tls {
        /// PEM certificate chain.
        cert: PathBuf,
        /// PEM private key.
        key: PathBuf,
    },
}

/// The address the API may listen on, once every rule has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindPlan {
    /// Where to listen.
    pub addr: SocketAddr,
    /// Whether `addr` is a loopback address (after canonicalizing an
    /// IPv4-mapped IPv6 address).
    pub loopback: bool,
    /// Plain or TLS.
    pub transport: Transport,
}

/// Why the API refuses to listen. Each is a startup failure, never a silent
/// downgrade to "no API".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindRefusal {
    /// `bind` is not an `IP:port` literal.
    #[error("api.bind '{0}' is not an IP:port address (hostnames are refused: they can resolve to a non-loopback address)")]
    InvalidAddress(String),
    /// `0.0.0.0` / `::`.
    #[error("api.bind {0} listens on every interface; bind one address instead")]
    Unspecified(SocketAddr),
    /// Non-loopback without `allow_lan`.
    #[error("api.bind {0} is not a loopback address; set api.allow_lan = true to expose the API beyond this host")]
    LanNotAllowed(SocketAddr),
    /// Non-loopback, no TLS, no tunnel acknowledgment.
    #[error("api.bind {0} would serve the bearer token in plaintext; configure api.tls_cert and api.tls_key, or set api.allow_plaintext_lan = true only if {0} is an encrypted tunnel (WireGuard/Tailscale) interface")]
    PlaintextLan(SocketAddr),
    /// One of `tls_cert` / `tls_key` without the other.
    #[error("api.tls_cert and api.tls_key must be set together")]
    TlsHalfConfigured,
    /// A limit that would make the API unusable or unbounded.
    #[error("api.{0} must be greater than zero")]
    ZeroLimit(&'static str),
}

impl ApiConfig {
    /// Decide whether and where to listen. `Ok(None)` when the API is
    /// disabled.
    ///
    /// # Errors
    ///
    /// A [`BindRefusal`] for every configuration that would expose more than
    /// the operator explicitly asked for (design §4, rules 2–6). The token
    /// (rule 7) is checked when the listener starts, since it is a file.
    pub fn bind_plan(&self) -> Result<Option<BindPlan>, BindRefusal> {
        if !self.enabled {
            return Ok(None);
        }
        for (name, value) in [
            ("max_upload_bytes", self.max_upload_bytes),
            ("max_audio_seconds", u64::from(self.max_audio_seconds)),
            ("max_connections", u64::from(self.max_connections)),
            ("max_ws_sessions", u64::from(self.max_ws_sessions)),
            ("requests_per_minute", u64::from(self.requests_per_minute)),
            ("burst", u64::from(self.burst)),
        ] {
            if value == 0 {
                return Err(BindRefusal::ZeroLimit(name));
            }
        }
        let addr: SocketAddr = self
            .bind
            .trim()
            .parse()
            .map_err(|_| BindRefusal::InvalidAddress(self.bind.clone()))?;
        let ip = canonical(addr.ip());
        if ip.is_unspecified() {
            return Err(BindRefusal::Unspecified(addr));
        }
        let loopback = ip.is_loopback();
        let transport = match (self.tls_cert.trim(), self.tls_key.trim()) {
            ("", "") => Transport::Plain,
            ("", _) | (_, "") => return Err(BindRefusal::TlsHalfConfigured),
            (cert, key) => Transport::Tls {
                cert: PathBuf::from(cert),
                key: PathBuf::from(key),
            },
        };
        if !loopback {
            if !self.allow_lan {
                return Err(BindRefusal::LanNotAllowed(addr));
            }
            if transport == Transport::Plain && !self.allow_plaintext_lan {
                return Err(BindRefusal::PlaintextLan(addr));
            }
        }
        Ok(Some(BindPlan {
            addr,
            loopback,
            transport,
        }))
    }

    /// Configuration errors, for `dictated --check-config` and startup.
    /// Nothing is reported for a disabled API: its keys cannot expose
    /// anything, so they must not stop the daemon.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        match self.bind_plan() {
            Ok(_) => Vec::new(),
            Err(refusal) => vec![refusal.to_string()],
        }
    }

    /// The token file this configuration names.
    #[must_use]
    pub fn token_path(&self) -> PathBuf {
        if self.token_file.trim().is_empty() {
            default_token_path()
        } else {
            PathBuf::from(self.token_file.trim())
        }
    }

    /// The protocol limits a network connection is held to.
    ///
    /// `max_message_bytes` is the *encoded* size of the largest upload:
    /// inline audio over the WebSocket is base64 (4/3) inside a JSON envelope.
    #[must_use]
    pub fn limits(&self) -> Limits {
        let encoded = self
            .max_upload_bytes
            .saturating_mul(4)
            .div_ceil(3)
            .saturating_add(4096);
        Limits {
            max_message_bytes: u32::try_from(encoded).unwrap_or(u32::MAX),
            max_audio_ms: Some(self.max_audio_seconds.saturating_mul(1000)),
            max_concurrent_sessions: 1,
            ..Limits::default()
        }
    }

    /// The capabilities every network connection is offered (design §7.1).
    ///
    /// Starts from the protocol's reference remote set, applies the one
    /// opt-in this slice has, and is clamped by [`network_ceiling`] whatever
    /// the configuration says.
    #[must_use]
    pub fn grant(&self) -> Capabilities {
        let mut caps = Capabilities::remote_transcription_only();
        caps.features.raw_text = self.expose_raw_text;
        // Not implemented in this build (design §11): binary frames are
        // refused and `begin_audio_stream` is `unsupported_command`.
        caps.features.streaming_audio = false;
        // Uploads carry no live audio, so there is no level to report.
        caps.features.audio_level_events = false;
        caps.limits = self.limits();
        network_ceiling(caps)
    }
}

/// Clear everything a network peer must never hold, whatever it was given.
///
/// The second of three locks on the network grant (the first is starting
/// from `remote_transcription_only`, the third is the daemon treating every
/// network connection as a non-owner, which withdraws config authority again).
/// A future config key that tried to widen the grant is undone here.
#[must_use]
pub fn network_ceiling(mut caps: Capabilities) -> Capabilities {
    let f = &mut caps.features;
    f.text_injection = false;
    f.host_capture = false;
    f.context_read = false;
    f.config_read = false;
    f.config_write = false;
    f.diagnostics = false;
    f.history_read = false;
    f.history_write = false;
    f.dictionary_read = false;
    f.dictionary_write = false;
    f.snippets_read = false;
    f.snippets_write = false;
    f.wake_word = false;
    caps.routes.retain(|route| *route == Route::Type);
    caps
}

/// `$XDG_DATA_HOME/dictated/api-token`.
#[must_use]
pub fn default_token_path() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/nonexistent"))
                .join(".local/share")
        })
        .join(DATA_DIR)
        .join(TOKEN_FILE)
}

/// An IPv4-mapped IPv6 address as the IPv4 address it carries, so
/// `::ffff:127.0.0.1` is loopback and `::ffff:0.0.0.0` is unspecified.
fn canonical(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled(bind: &str) -> ApiConfig {
        ApiConfig {
            enabled: true,
            bind: bind.into(),
            ..ApiConfig::default()
        }
    }

    #[test]
    fn the_default_opens_no_port_at_all() {
        let config = ApiConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.bind_plan(), Ok(None));
        assert!(config.validate().is_empty());
    }

    #[test]
    fn enabling_it_binds_loopback_in_plaintext() {
        let plan = enabled(&ApiConfig::default().bind)
            .bind_plan()
            .unwrap()
            .unwrap();
        assert!(plan.loopback);
        assert_eq!(plan.addr.ip().to_string(), "127.0.0.1");
        assert_eq!(plan.addr.port(), DEFAULT_PORT);
        assert_eq!(plan.transport, Transport::Plain);
    }

    #[test]
    fn every_loopback_spelling_is_loopback() {
        for bind in [
            "127.0.0.1:0",
            "127.9.9.9:1",
            "[::1]:7313",
            "[::ffff:127.0.0.1]:7313",
        ] {
            let plan = enabled(bind).bind_plan().unwrap().unwrap();
            assert!(plan.loopback, "{bind}");
        }
    }

    #[test]
    fn a_lan_address_without_the_opt_in_is_refused() {
        for bind in [
            "192.168.1.20:7313",
            "10.0.0.5:7313",
            "100.64.1.2:7313",
            "[fe80::1]:7313",
            "[::ffff:192.168.1.20]:7313",
        ] {
            assert!(
                matches!(
                    enabled(bind).bind_plan(),
                    Err(BindRefusal::LanNotAllowed(_))
                ),
                "{bind}"
            );
        }
    }

    #[test]
    fn a_lan_opt_in_still_refuses_plaintext() {
        let config = ApiConfig {
            allow_lan: true,
            ..enabled("192.168.1.20:7313")
        };
        assert!(matches!(
            config.bind_plan(),
            Err(BindRefusal::PlaintextLan(_))
        ));
    }

    #[test]
    fn a_lan_bind_is_allowed_with_tls() {
        let config = ApiConfig {
            allow_lan: true,
            tls_cert: "/etc/x/cert.pem".into(),
            tls_key: "/etc/x/key.pem".into(),
            ..enabled("192.168.1.20:7313")
        };
        let plan = config.bind_plan().unwrap().unwrap();
        assert!(!plan.loopback);
        assert!(matches!(plan.transport, Transport::Tls { .. }));
    }

    #[test]
    fn a_tunnel_bind_is_allowed_in_plaintext_only_when_acknowledged() {
        let config = ApiConfig {
            allow_lan: true,
            allow_plaintext_lan: true,
            ..enabled("100.101.102.103:7313")
        };
        let plan = config.bind_plan().unwrap().unwrap();
        assert!(!plan.loopback);
        assert_eq!(plan.transport, Transport::Plain);
        // The acknowledgment alone is not the opt-in.
        let no_lan = ApiConfig {
            allow_lan: false,
            ..config
        };
        assert!(matches!(
            no_lan.bind_plan(),
            Err(BindRefusal::LanNotAllowed(_))
        ));
    }

    #[test]
    fn every_interface_is_refused_even_with_every_opt_in() {
        for bind in ["0.0.0.0:7313", "[::]:7313", "[::ffff:0.0.0.0]:7313"] {
            let config = ApiConfig {
                allow_lan: true,
                allow_plaintext_lan: true,
                tls_cert: "c".into(),
                tls_key: "k".into(),
                ..enabled(bind)
            };
            assert!(
                matches!(config.bind_plan(), Err(BindRefusal::Unspecified(_))),
                "{bind}"
            );
        }
    }

    #[test]
    fn hostnames_and_garbage_are_refused() {
        for bind in ["localhost:7313", "desk.lan:7313", "127.0.0.1", "", "7313"] {
            assert!(
                matches!(
                    enabled(bind).bind_plan(),
                    Err(BindRefusal::InvalidAddress(_))
                ),
                "{bind:?}"
            );
        }
    }

    #[test]
    fn half_a_tls_configuration_is_refused() {
        for (cert, key) in [("c.pem", ""), ("", "k.pem")] {
            let config = ApiConfig {
                tls_cert: cert.into(),
                tls_key: key.into(),
                ..enabled("127.0.0.1:0")
            };
            assert_eq!(config.bind_plan(), Err(BindRefusal::TlsHalfConfigured));
        }
    }

    #[test]
    fn zero_limits_are_refused() {
        let config = ApiConfig {
            max_upload_bytes: 0,
            ..enabled("127.0.0.1:0")
        };
        assert_eq!(
            config.bind_plan(),
            Err(BindRefusal::ZeroLimit("max_upload_bytes"))
        );
    }

    #[test]
    fn a_disabled_api_reports_no_errors_whatever_it_says() {
        let config = ApiConfig {
            enabled: false,
            bind: "0.0.0.0:1".into(),
            ..ApiConfig::default()
        };
        assert!(config.validate().is_empty());
        assert!(!enabled("0.0.0.0:1").validate().is_empty());
    }

    #[test]
    fn the_grant_is_transcription_only() {
        let grant = ApiConfig::default().grant();
        let f = &grant.features;
        assert!(f.transcribe_upload);
        assert!(!f.raw_text, "raw_text is opt-in for network peers");
        assert!(!f.streaming_audio, "not implemented in this build");
        for (name, on) in [
            ("text_injection", f.text_injection),
            ("host_capture", f.host_capture),
            ("context_read", f.context_read),
            ("config_read", f.config_read),
            ("config_write", f.config_write),
            ("diagnostics", f.diagnostics),
            ("history_read", f.history_read),
            ("history_write", f.history_write),
            ("dictionary_read", f.dictionary_read),
            ("dictionary_write", f.dictionary_write),
            ("snippets_read", f.snippets_read),
            ("snippets_write", f.snippets_write),
        ] {
            assert!(!on, "{name} must never reach a network peer");
        }
        assert_eq!(grant.routes, vec![Route::Type]);
        assert_eq!(grant.limits.max_audio_ms, Some(300_000));
    }

    #[test]
    fn expose_raw_text_is_the_only_widening_and_it_is_narrow() {
        let grant = ApiConfig {
            expose_raw_text: true,
            ..ApiConfig::default()
        }
        .grant();
        assert!(grant.features.raw_text);
        assert!(!grant.features.config_write);
    }

    #[test]
    fn the_ceiling_undoes_a_full_local_grant() {
        let clamped = network_ceiling(Capabilities::local_trusted());
        let f = &clamped.features;
        assert!(!f.text_injection && !f.host_capture && !f.context_read);
        assert!(!f.config_read && !f.config_write && !f.diagnostics);
        assert!(!f.history_read && !f.history_write);
        assert!(!f.dictionary_read && !f.dictionary_write);
        assert_eq!(clamped.routes, vec![Route::Type]);
    }

    #[test]
    fn upload_limits_cover_the_base64_envelope() {
        let config = ApiConfig {
            max_upload_bytes: 3_000,
            max_audio_seconds: 7,
            ..ApiConfig::default()
        };
        let limits = config.limits();
        assert_eq!(limits.max_message_bytes, 4_000 + 4_096);
        assert_eq!(limits.max_audio_ms, Some(7_000));
        assert_eq!(limits.max_concurrent_sessions, 1);
    }

    #[test]
    fn the_token_lives_under_this_daemons_data_dir_by_default() {
        let path = ApiConfig::default().token_path();
        assert!(path.ends_with("dictated/api-token"), "{}", path.display());
        let named = ApiConfig {
            token_file: "/run/x/token".into(),
            ..ApiConfig::default()
        };
        assert_eq!(named.token_path(), PathBuf::from("/run/x/token"));
    }

    #[test]
    fn unknown_keys_stay_unknown_and_partial_tables_take_defaults() {
        let config: ApiConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(config.enabled);
        assert_eq!(config.bind, ApiConfig::default().bind);
    }
}
