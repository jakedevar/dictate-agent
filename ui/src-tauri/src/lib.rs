//! dictate-agent's desktop UI (S32): an optional `dictate-proto` client.
//!
//! The daemon is fully functional without it. This crate talks to `dictated`
//! over its unix socket and nothing else: no network, no telemetry, and no
//! dependency on the daemon's own crates beyond the wire contract.

pub mod bridge;
pub mod hud;
pub mod icons;

pub mod app;
pub mod commands;
#[cfg(target_os = "linux")]
pub mod x11;
