//! The daemon's side of the network API (S33).
//!
//! `dictate-server` owns the transport — bind policy, TLS, the bearer token,
//! `Host`/`Origin` checks, limits. This module hands it a [`Backend`] whose
//! connections are [`NetworkConnection`]s: the unix socket's own dispatch, with
//! the network grant computed here.
//!
//! See `thoughts/shared/plans/2026-10-07-s33-network-api-security-design.md`.

use std::sync::Arc;

use dictate_proto::{Capabilities, Command, CommandResult, Event, Message, ProtoError};
use dictate_server::{ApiConfig, Backend, BoxFuture, Hangup, Session};

use crate::server::{NetworkConnection, ServerDeps};

/// What a network connection is offered (design §7.1).
///
/// The `[api]` grant (transcription only, `raw_text` opt-in), with the
/// daemon's informational flags mirrored and every upload limit no larger
/// than the local one, then clamped by the network ceiling. The connection
/// is also never the owner, so the shared handshake withdraws config
/// authority once more.
#[must_use]
pub fn network_grant(api: &ApiConfig, local: &Capabilities) -> Capabilities {
    let mut grant = api.grant();
    grant.features.headless = local.features.headless;
    grant.features.privacy_mode = local.features.privacy_mode;
    grant.limits.max_message_bytes = grant
        .limits
        .max_message_bytes
        .min(local.limits.max_message_bytes);
    grant.limits.max_audio_ms = match (grant.limits.max_audio_ms, local.limits.max_audio_ms) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    dictate_server::network_ceiling(grant)
}

/// Opens [`NetworkConnection`]s for `dictate-server`.
pub struct NetworkBackend {
    deps: Arc<ServerDeps>,
    grant: Capabilities,
}

impl NetworkBackend {
    /// A backend serving `deps` with the grant `api` implies.
    #[must_use]
    pub fn new(deps: Arc<ServerDeps>, api: &ApiConfig) -> Self {
        let grant = network_grant(api, &deps.capabilities);
        Self { deps, grant }
    }

    /// The capabilities each connection is offered.
    #[must_use]
    pub fn grant(&self) -> &Capabilities {
        &self.grant
    }
}

impl Backend for NetworkBackend {
    fn open(&self) -> Box<dyn Session> {
        Box::new(NetworkConnection::open(
            self.deps.clone(),
            self.grant.clone(),
        ))
    }
}

impl Session for NetworkConnection {
    fn handle_text<'a>(&'a mut self, text: &'a str, hangup: Hangup) -> BoxFuture<'a, Message> {
        Box::pin(NetworkConnection::handle_line(self, text, hangup))
    }

    fn execute(
        &mut self,
        command: Command,
        hangup: Hangup,
    ) -> BoxFuture<'_, Result<CommandResult, ProtoError>> {
        Box::pin(NetworkConnection::execute(self, command, hangup))
    }

    fn next_event(&mut self) -> BoxFuture<'_, Option<Event>> {
        Box::pin(NetworkConnection::next_event(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(caps: &Capabilities) -> serde_json::Map<String, serde_json::Value> {
        match serde_json::to_value(&caps.features).unwrap() {
            serde_json::Value::Object(map) => map,
            other => panic!("features serialize as an object, got {other}"),
        }
    }

    /// Design §2 T2: a same-user process already has the unix socket, so the
    /// network API must never grant anything that socket does not.
    #[test]
    fn the_network_grant_is_a_strict_subset_of_the_local_grant() {
        let local = crate::server::local_capabilities(true);
        for api in [
            ApiConfig::default(),
            ApiConfig {
                expose_raw_text: true,
                max_upload_bytes: u64::MAX,
                max_audio_seconds: u32::MAX,
                ..ApiConfig::default()
            },
        ] {
            let grant = network_grant(&api, &local);
            let (network, local_flags) = (flags(&grant), flags(&local));
            for (name, on) in &network {
                // `headless` and `privacy_mode` describe the daemon, not a
                // permission; they are mirrored, not granted.
                if matches!(name.as_str(), "headless" | "privacy_mode") {
                    assert_eq!(on, &local_flags[name], "{name} mirrors the daemon");
                    continue;
                }
                if on == &serde_json::Value::Bool(true) {
                    assert_eq!(
                        local_flags.get(name),
                        Some(&serde_json::Value::Bool(true)),
                        "the network grant holds '{name}', which the socket does not"
                    );
                }
            }
            assert!(
                local_flags.iter().filter(|(_, v)| **v == true).count()
                    > network.iter().filter(|(_, v)| **v == true).count(),
                "strictly smaller"
            );
            for route in &grant.routes {
                assert!(local.allows_route(route));
            }
            assert!(grant.limits.max_message_bytes <= local.limits.max_message_bytes);
            assert!(grant.limits.max_audio_ms <= local.limits.max_audio_ms);
            assert!(!grant.features.config_write && !grant.features.config_read);
            assert!(!grant.features.text_injection && !grant.features.host_capture);
        }
    }

    #[test]
    fn the_daemons_informational_flags_are_mirrored() {
        let mut local = crate::server::local_capabilities(true);
        local.features.privacy_mode = true;
        local.features.headless = true;
        let grant = network_grant(&ApiConfig::default(), &local);
        assert!(grant.features.privacy_mode);
        assert!(grant.features.headless);
    }
}
