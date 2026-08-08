//! The signal shim: SIGUSR1 and SIGUSR2, routed into the protocol's own
//! command handlers.
//!
//! Jake's keybindings run `scripts/dictate-toggle`, which signals a PID. That
//! has to keep working, and it has to keep working *identically* — a daemon
//! where the hotkey and the socket take different code paths is a daemon with
//! two state machines that agree until the day they don't.
//!
//! So this module contains no recording logic at all. It translates a signal
//! into an [`Actor::Signal`] and one engine call:
//!
//! | Signal | Engine call | Protocol equivalent |
//! |---|---|---|
//! | `SIGUSR1` | [`EngineHandle::toggle`] | `start_dictation` / `stop` |
//! | `SIGUSR2` | [`EngineHandle::cancel`] | `cancel` |
//! | `SIGINT`/`SIGTERM` | shutdown | — |
//!
//! [`EngineHandle::toggle`] is not a fourth verb: it inspects the state inside
//! the engine task and calls the same `handle_start` / `handle_stop` that
//! `start_dictation` and `stop` call. Toggling *inside* the engine is also what
//! makes the signal path race-free — a client doing `get_status` then
//! `start_dictation` has a window between the two where another actor can act,
//! and gets `busy` if it loses; the signal path never opens that window.
//!
//! # Why signals outrank connections
//!
//! [`Actor::Signal`] may control any session, including one a connected client
//! owns. The hotkey is the physical control surface of the machine the
//! microphone is plugged into: if Jake hits cancel, the recording stops, and no
//! remote client gets a veto over that.

use dictate_core::session::Actor;
use dictate_core::{EngineHandle, ResolvedOptions};
use signal_hook::consts::signal::{SIGINT, SIGTERM, SIGUSR1, SIGUSR2};
use signal_hook_tokio::Signals;
use tokio_stream::StreamExt;
use tracing::{info, warn};

/// What a delivered signal means to the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalAction {
    /// Toggle recording (SIGUSR1).
    Toggle,
    /// Cancel the active session (SIGUSR2).
    Cancel,
    /// Wind down (SIGINT, SIGTERM).
    Shutdown,
    /// Something we do not subscribe to.
    Ignore,
}

/// Classify a signal number.
///
/// Split out from delivery so the mapping is testable without sending real
/// signals to the test process.
#[must_use]
pub fn classify(signal: i32) -> SignalAction {
    match signal {
        SIGUSR1 => SignalAction::Toggle,
        SIGUSR2 => SignalAction::Cancel,
        SIGINT | SIGTERM => SignalAction::Shutdown,
        _ => SignalAction::Ignore,
    }
}

/// Apply a signal to the engine.
///
/// Returns `true` when the daemon should shut down. Every branch here is a
/// call the socket dispatcher also makes; there is deliberately no third
/// implementation of "start recording" in this file.
pub async fn apply(action: SignalAction, engine: &EngineHandle, options: &ResolvedOptions) -> bool {
    match action {
        SignalAction::Toggle => {
            match engine.toggle(Actor::Signal, options.clone()).await {
                Ok(outcome) => info!(?outcome, "SIGUSR1"),
                // A failure here is normal, not exceptional: SIGUSR1 arriving
                // mid-transcription is `busy`, and the right response is to
                // say so rather than to queue a second session.
                Err(e) => warn!("SIGUSR1 refused: {} ({})", e.message, e.code.as_str()),
            }
            false
        }
        SignalAction::Cancel => {
            match engine.cancel(Actor::Signal).await {
                Ok(id) => info!(session = %id.as_str(), "SIGUSR2"),
                Err(e) => warn!("SIGUSR2 refused: {} ({})", e.message, e.code.as_str()),
            }
            false
        }
        SignalAction::Shutdown => true,
        SignalAction::Ignore => false,
    }
}

/// Listen for signals until a shutdown signal arrives.
///
/// # Errors
///
/// If the signal handlers cannot be installed.
pub async fn listen(engine: EngineHandle, options: ResolvedOptions) -> anyhow::Result<()> {
    let mut signals = Signals::new([SIGUSR1, SIGUSR2, SIGINT, SIGTERM])?;
    info!("signal shim active (SIGUSR1 toggle, SIGUSR2 cancel)");
    while let Some(signal) = signals.next().await {
        if apply(classify(signal), &engine, &options).await {
            info!("shutdown signal received");
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scripts_signals_map_to_the_protocols_verbs() {
        // `scripts/dictate-toggle` sends SIGUSR1; `scripts/dictate-cancel`
        // sends SIGUSR2. These two lines are the contract with those scripts.
        assert_eq!(classify(SIGUSR1), SignalAction::Toggle);
        assert_eq!(classify(SIGUSR2), SignalAction::Cancel);
    }

    #[test]
    fn termination_signals_shut_the_daemon_down() {
        assert_eq!(classify(SIGINT), SignalAction::Shutdown);
        assert_eq!(classify(SIGTERM), SignalAction::Shutdown);
    }

    #[test]
    fn unsubscribed_signals_are_ignored() {
        assert_eq!(classify(signal_hook::consts::signal::SIGHUP), SignalAction::Ignore);
    }
}
