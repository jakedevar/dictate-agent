//! Tray icons, drawn at runtime.
//!
//! A status dot whose colour and shape carry the daemon's state. Drawn rather
//! than shipped as files so the states cannot drift out of sync with the
//! assets, and so each is distinguishable without colour (filled vs ring vs
//! ring-with-dot) for colour-blind users.

use dictate_proto::State;

/// What the tray shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    /// The daemon is not reachable.
    Disconnected,
    /// Connected, nothing happening.
    Idle,
    /// Listening.
    Recording,
    /// Transcribing, formatting or injecting.
    Processing,
    /// The last session failed.
    Error,
}

impl TrayState {
    /// The tray state for a session entering `state`.
    #[must_use]
    pub fn from_session(state: &State) -> Self {
        match state {
            State::Recording => Self::Recording,
            State::Transcribing | State::Formatting | State::Injecting => Self::Processing,
            State::Error => Self::Error,
            _ => Self::Idle,
        }
    }

    /// Tooltip text.
    #[must_use]
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Disconnected => "dictate: daemon not running",
            Self::Idle => "dictate: ready",
            Self::Recording => "dictate: listening",
            Self::Processing => "dictate: transcribing",
            Self::Error => "dictate: last dictation failed",
        }
    }
}

/// Edge length of the generated icon, in pixels.
pub const SIZE: u32 = 32;

/// RGBA pixels for `state`, `SIZE`×`SIZE`.
#[must_use]
pub fn rgba(state: TrayState) -> Vec<u8> {
    const GREY: [u8; 3] = [0x9c, 0xa3, 0xaf];
    const LIGHT: [u8; 3] = [0xe5, 0xe7, 0xeb];
    const RED: [u8; 3] = [0xef, 0x44, 0x44];
    const AMBER: [u8; 3] = [0xf5, 0x9e, 0x0b];
    // (outer radius, ring width or None for filled, colour, centre dot)
    let (ring, color, dot): (Option<f64>, [u8; 3], bool) = match state {
        TrayState::Disconnected => (Some(3.0), GREY, false),
        TrayState::Idle => (None, LIGHT, false),
        TrayState::Recording => (None, RED, false),
        TrayState::Processing => (Some(4.0), AMBER, true),
        TrayState::Error => (Some(3.0), RED, true),
    };
    let size = SIZE as usize;
    let c = f64::from(SIZE) / 2.0;
    let outer = c - 3.0;
    let mut px = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f64 + 0.5 - c).powi(2) + (y as f64 + 0.5 - c).powi(2)).sqrt();
            // Anti-aliased coverage of the disc, minus the hole for rings.
            let disc = (outer + 0.5 - d).clamp(0.0, 1.0);
            let hole = ring.map_or(0.0, |w| (outer - w + 0.5 - d).clamp(0.0, 1.0));
            let centre = if dot { (4.5 - d).clamp(0.0, 1.0) } else { 0.0 };
            let alpha = (disc - hole).max(centre);
            let i = (y * size + x) * 4;
            px[i..i + 3].copy_from_slice(&color);
            px[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(px: &[u8], x: usize, y: usize) -> u8 {
        px[(y * SIZE as usize + x) * 4 + 3]
    }

    #[test]
    fn every_state_has_a_distinct_icon() {
        let all = [
            TrayState::Disconnected,
            TrayState::Idle,
            TrayState::Recording,
            TrayState::Processing,
            TrayState::Error,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(rgba(*a), rgba(*b), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn shape_not_only_colour_separates_states() {
        let c = SIZE as usize / 2;
        // Filled: opaque centre. Ring: transparent centre. Ring+dot: opaque centre.
        assert_eq!(alpha_at(&rgba(TrayState::Recording), c, c), 255);
        assert_eq!(alpha_at(&rgba(TrayState::Disconnected), c, c), 0);
        assert_eq!(alpha_at(&rgba(TrayState::Error), c, c), 255);
        // Corners are transparent everywhere.
        assert_eq!(alpha_at(&rgba(TrayState::Idle), 0, 0), 0);
        assert_eq!(rgba(TrayState::Idle).len(), (SIZE * SIZE * 4) as usize);
    }

    #[test]
    fn session_states_map_to_tray_states() {
        assert_eq!(TrayState::from_session(&State::Recording), TrayState::Recording);
        assert_eq!(TrayState::from_session(&State::Formatting), TrayState::Processing);
        assert_eq!(TrayState::from_session(&State::Error), TrayState::Error);
        assert_eq!(TrayState::from_session(&State::Done), TrayState::Idle);
        assert_eq!(TrayState::from_session(&State::Cancelled), TrayState::Idle);
    }
}
