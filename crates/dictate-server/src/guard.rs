//! Request admission: `Host`, `Origin`, and the bearer token (design §2 T3, §5).
//!
//! Pure functions over header values, so each rule is tested on its own; the
//! middleware in [`crate::http`] applies them in order before any handler or
//! WebSocket upgrade runs.

use std::collections::HashSet;
use std::net::IpAddr;

use crate::config::BindPlan;

/// The `Host` values a request may carry.
#[derive(Debug, Clone)]
pub struct HostPolicy {
    allowed: HashSet<String>,
}

impl HostPolicy {
    /// The bound address's own spellings, plus `extra` from `[api]
    /// allowed_hosts`. On loopback `localhost:<port>` is accepted too; it
    /// cannot be produced by a DNS-rebinding page, whose `Host` is its own
    /// name.
    #[must_use]
    pub fn new(plan: &BindPlan, extra: &[String]) -> Self {
        let port = plan.addr.port();
        let mut allowed = HashSet::new();
        let ip = plan.addr.ip();
        allowed.insert(host_with_port(ip, port));
        let canonical = ip.to_canonical();
        allowed.insert(host_with_port(canonical, port));
        if plan.loopback {
            allowed.insert(format!("localhost:{port}"));
        }
        for host in extra {
            let host = host.trim().to_ascii_lowercase();
            if !host.is_empty() {
                allowed.insert(host);
            }
        }
        Self { allowed }
    }

    /// Whether a request with this `Host` header may proceed. A missing
    /// header is refused: every HTTP/1.1 client sends one.
    #[must_use]
    pub fn allows(&self, host: Option<&str>) -> bool {
        host.map(|h| h.trim().to_ascii_lowercase())
            .is_some_and(|h| self.allowed.contains(&h))
    }
}

fn host_with_port(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(v4) => format!("{v4}:{port}"),
        IpAddr::V6(v6) => format!("[{v6}]:{port}"),
    }
}

/// Whether a request with this `Origin` header may proceed: only when it has
/// none (a native client) or names an explicitly allowed origin. Every
/// browser sends `Origin` on a WebSocket handshake and on cross-origin
/// requests, so the default — no allowed origins — refuses all of them,
/// including the literal `null` origin of sandboxed pages.
#[must_use]
pub fn origin_allowed(origin: Option<&str>, allowed: &[String]) -> bool {
    match origin {
        None => true,
        Some(origin) => {
            let origin = origin.trim();
            allowed
                .iter()
                .any(|a| a.trim().eq_ignore_ascii_case(origin) && !a.trim().is_empty())
        }
    }
}

/// Why a request carried no usable credential. Logged as a class, never with
/// the value presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    /// No `Authorization` header.
    Missing,
    /// Not `Bearer <token>`, or not valid UTF-8.
    Malformed,
    /// A well-formed bearer credential that is not the token.
    Mismatch,
}

impl AuthFailure {
    /// The class name for logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Malformed => "malformed",
            Self::Mismatch => "mismatch",
        }
    }
}

/// The token in an `Authorization` header value: `Bearer <token>`, scheme
/// case-insensitive, exactly one credential.
///
/// # Errors
///
/// [`AuthFailure::Missing`] or [`AuthFailure::Malformed`].
pub fn bearer(header: Option<&[u8]>) -> Result<&str, AuthFailure> {
    let raw = header.ok_or(AuthFailure::Missing)?;
    let value = std::str::from_utf8(raw).map_err(|_| AuthFailure::Malformed)?;
    let (scheme, token) = value.trim().split_once(' ').ok_or(AuthFailure::Malformed)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(AuthFailure::Malformed);
    }
    let token = token.trim();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return Err(AuthFailure::Malformed);
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiConfig, Transport};

    fn plan(bind: &str, lan: bool) -> BindPlan {
        ApiConfig {
            enabled: true,
            bind: bind.into(),
            allow_lan: lan,
            allow_plaintext_lan: lan,
            ..ApiConfig::default()
        }
        .bind_plan()
        .unwrap()
        .unwrap()
    }

    #[test]
    fn loopback_accepts_its_own_names_only() {
        let p = HostPolicy::new(&plan("127.0.0.1:7313", false), &[]);
        assert!(p.allows(Some("127.0.0.1:7313")));
        assert!(p.allows(Some("LOCALHOST:7313")));
        // DNS rebinding: the attacker's name, resolved to 127.0.0.1.
        assert!(!p.allows(Some("evil.example:7313")));
        assert!(!p.allows(Some("evil.example")));
        // Wrong port, missing port, missing header.
        assert!(!p.allows(Some("127.0.0.1:80")));
        assert!(!p.allows(Some("127.0.0.1")));
        assert!(!p.allows(None));
    }

    #[test]
    fn ipv6_loopback_is_bracketed() {
        let p = HostPolicy::new(&plan("[::1]:9000", false), &[]);
        assert!(p.allows(Some("[::1]:9000")));
        assert!(p.allows(Some("localhost:9000")));
        assert!(!p.allows(Some("::1:9000")));
    }

    #[test]
    fn a_lan_bind_accepts_its_address_and_named_extras_not_localhost() {
        let plan = plan("100.101.102.103:7313", true);
        assert_eq!(plan.transport, Transport::Plain);
        let p = HostPolicy::new(&plan, &["Desk.Tail1234.ts.net".into()]);
        assert!(p.allows(Some("100.101.102.103:7313")));
        assert!(p.allows(Some("desk.tail1234.ts.net")));
        assert!(!p.allows(Some("localhost:7313")));
    }

    #[test]
    fn any_origin_is_refused_by_default() {
        assert!(origin_allowed(None, &[]));
        for origin in ["http://evil.example", "null", "http://127.0.0.1:7313", ""] {
            assert!(!origin_allowed(Some(origin), &[]), "{origin:?}");
        }
        let allowed = vec!["https://ui.example".to_string()];
        assert!(origin_allowed(Some("https://UI.example"), &allowed));
        assert!(!origin_allowed(Some("https://ui.example.evil"), &allowed));
        // An empty entry allows nothing.
        assert!(!origin_allowed(Some(""), &[String::new()]));
    }

    #[test]
    fn bearer_parsing_is_strict() {
        assert_eq!(bearer(Some(b"Bearer dct1_x")), Ok("dct1_x"));
        assert_eq!(bearer(Some(b"bearer  dct1_x ")), Ok("dct1_x"));
        assert_eq!(bearer(None), Err(AuthFailure::Missing));
        for bad in [
            &b"Basic dXNlcjpwYXNz"[..],
            b"Bearer",
            b"Bearer ",
            b"dct1_x",
            b"Bearer a b",
            b"Bearer \xff\xfe",
        ] {
            assert_eq!(bearer(Some(bad)), Err(AuthFailure::Malformed), "{bad:?}");
        }
    }
}
