//! The Flow bar's behavior, separate from any window.
//!
//! [`HudVisibility`] decides *when* the bar is on screen from the daemon's
//! `state_changed` events; [`placement`] decides *where*. Both are pure and
//! unit-tested; `app.rs` applies their decisions to the real window, and
//! `x11.rs` owns the layering mechanism (never taking focus, click-through).

use std::time::{Duration, Instant};

use dictate_proto::State;

/// How long a finished session's result stays on screen.
///
/// About a second for success and cancellation, as the brief asks; an error
/// stays longer because it carries a message someone has to be able to read.
#[must_use]
pub fn linger(state: &State) -> Duration {
    match state {
        State::Error => Duration::from_millis(2400),
        State::Cancelled => Duration::from_millis(800),
        _ => Duration::from_millis(1100),
    }
}

/// What the window should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudAction {
    /// Position under the pointer and map the window (if not already shown).
    Show,
    /// Unmap the window.
    Hide,
    /// Leave it as it is.
    Keep,
}

/// Show on activity, linger on a result, hide when idle.
#[derive(Debug, Default)]
pub struct HudVisibility {
    shown: bool,
    hide_at: Option<Instant>,
}

impl HudVisibility {
    /// Whether the bar is (meant to be) on screen.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// When a pending hide falls due.
    #[must_use]
    pub fn hide_at(&self) -> Option<Instant> {
        self.hide_at
    }

    /// React to a session entering `state`.
    pub fn on_state(&mut self, state: &State, now: Instant) -> HudAction {
        match state {
            State::Recording | State::Transcribing | State::Formatting | State::Injecting => {
                self.hide_at = None;
                if self.shown {
                    HudAction::Keep
                } else {
                    self.shown = true;
                    HudAction::Show
                }
            }
            // A result is only worth showing if the bar was already up: a
            // stray terminal event must not flash a window over the user's work.
            State::Done | State::Error | State::Cancelled => {
                if self.shown {
                    self.hide_at = Some(now + linger(state));
                }
                HudAction::Keep
            }
            State::Idle => {
                if self.shown && self.hide_at.is_none() {
                    self.hide_at = Some(now + linger(&State::Done));
                }
                HudAction::Keep
            }
            // A state this build does not know: change nothing.
            _ => HudAction::Keep,
        }
    }

    /// Advance the clock.
    pub fn on_tick(&mut self, now: Instant) -> HudAction {
        match self.hide_at {
            Some(at) if now >= at => self.hide_now(),
            _ => HudAction::Keep,
        }
    }

    /// The daemon went away: nothing is in progress any more.
    pub fn hide_now(&mut self) -> HudAction {
        self.hide_at = None;
        if std::mem::take(&mut self.shown) {
            HudAction::Hide
        } else {
            HudAction::Keep
        }
    }
}

/// Where the bar goes. All coordinates are physical pixels.
pub mod placement {
    /// A monitor's rectangle and scale factor.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct Monitor {
        /// Left edge.
        pub x: i32,
        /// Top edge.
        pub y: i32,
        /// Width.
        pub width: u32,
        /// Height.
        pub height: u32,
        /// Physical pixels per logical pixel.
        pub scale: f64,
    }

    impl Monitor {
        fn contains(&self, px: f64, py: f64) -> bool {
            px >= f64::from(self.x)
                && py >= f64::from(self.y)
                && px < f64::from(self.x) + f64::from(self.width)
                && py < f64::from(self.y) + f64::from(self.height)
        }
    }

    /// The monitor containing the pointer, else the first one.
    #[must_use]
    pub fn monitor_under(pointer: Option<(f64, f64)>, monitors: &[Monitor]) -> Option<Monitor> {
        pointer
            .and_then(|(x, y)| monitors.iter().find(|m| m.contains(x, y)))
            .or_else(|| monitors.first())
            .copied()
    }

    /// Top-left corner that centers a `logical`-sized bar horizontally,
    /// `bottom_margin` logical pixels above the monitor's bottom edge.
    ///
    /// Scale is applied to the bar's size and the margin alike, so the bar
    /// sits at the same visual distance from the edge on a 1x and a 2x screen.
    #[must_use]
    pub fn bottom_center(monitor: &Monitor, logical: (f64, f64), bottom_margin: f64) -> (i32, i32) {
        let w = logical.0 * monitor.scale;
        let h = logical.1 * monitor.scale;
        let margin = bottom_margin * monitor.scale;
        let x = f64::from(monitor.x) + (f64::from(monitor.width) - w) / 2.0;
        let y = f64::from(monitor.y) + f64::from(monitor.height) - h - margin;
        // Clamp so a tiny or oddly scaled monitor never places it off-screen.
        let max_y = f64::from(monitor.y) + (f64::from(monitor.height) - h).max(0.0);
        (x.round() as i32, y.clamp(f64::from(monitor.y), max_y).round() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::placement::{bottom_center, monitor_under, Monitor};
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn recording_shows_once_and_later_stages_keep_it_up() {
        let mut v = HudVisibility::default();
        let now = t0();
        assert_eq!(v.on_state(&State::Recording, now), HudAction::Show);
        assert_eq!(v.on_state(&State::Transcribing, now), HudAction::Keep);
        assert_eq!(v.on_state(&State::Formatting, now), HudAction::Keep);
        assert!(v.is_shown());
        assert_eq!(v.on_tick(now + Duration::from_secs(60)), HudAction::Keep);
    }

    #[test]
    fn a_result_lingers_then_hides() {
        let mut v = HudVisibility::default();
        let now = t0();
        v.on_state(&State::Recording, now);
        assert_eq!(v.on_state(&State::Done, now), HudAction::Keep);
        assert_eq!(v.on_tick(now + Duration::from_millis(1000)), HudAction::Keep);
        assert_eq!(v.on_tick(now + Duration::from_millis(1100)), HudAction::Hide);
        assert!(!v.is_shown());
        assert_eq!(v.on_tick(now + Duration::from_secs(5)), HudAction::Keep, "hides once");
    }

    #[test]
    fn errors_stay_long_enough_to_read_and_cancel_is_brief() {
        assert!(linger(&State::Error) > linger(&State::Done));
        assert!(linger(&State::Cancelled) < linger(&State::Done));
        assert!(linger(&State::Done) >= Duration::from_millis(900));
    }

    #[test]
    fn a_new_session_during_the_linger_cancels_the_hide() {
        let mut v = HudVisibility::default();
        let now = t0();
        v.on_state(&State::Recording, now);
        v.on_state(&State::Done, now);
        assert_eq!(v.on_state(&State::Recording, now + Duration::from_millis(300)), HudAction::Keep);
        assert_eq!(v.on_tick(now + Duration::from_secs(10)), HudAction::Keep);
        assert!(v.is_shown());
    }

    #[test]
    fn a_stray_terminal_event_never_flashes_the_bar() {
        let mut v = HudVisibility::default();
        let now = t0();
        assert_eq!(v.on_state(&State::Done, now), HudAction::Keep);
        assert_eq!(v.on_state(&State::Error, now), HudAction::Keep);
        assert_eq!(v.on_tick(now + Duration::from_secs(5)), HudAction::Keep);
        assert!(!v.is_shown());
    }

    #[test]
    fn losing_the_daemon_hides_immediately() {
        let mut v = HudVisibility::default();
        v.on_state(&State::Recording, t0());
        assert_eq!(v.hide_now(), HudAction::Hide);
        assert_eq!(v.hide_now(), HudAction::Keep);
    }

    #[test]
    fn unknown_states_change_nothing() {
        let mut v = HudVisibility::default();
        assert_eq!(v.on_state(&State::from("teleporting".to_string()), t0()), HudAction::Keep);
        assert!(!v.is_shown());
    }

    const ONE_X: Monitor = Monitor { x: 0, y: 0, width: 1920, height: 1080, scale: 1.0 };
    const TWO_X: Monitor = Monitor { x: 1920, y: 0, width: 3840, height: 2160, scale: 2.0 };

    #[test]
    fn bottom_center_at_1x() {
        assert_eq!(bottom_center(&ONE_X, (240.0, 64.0), 56.0), (840, 960));
    }

    #[test]
    fn bottom_center_scales_size_and_margin_on_hidpi() {
        // 480x128 physical, 112 physical margin, on a monitor offset by 1920.
        assert_eq!(bottom_center(&TWO_X, (240.0, 64.0), 56.0), (1920 + 1680, 1920));
    }

    #[test]
    fn negative_origins_and_tiny_monitors_stay_on_screen() {
        let left = Monitor { x: -1280, y: -200, width: 1280, height: 100, scale: 1.0 };
        let (x, y) = bottom_center(&left, (240.0, 64.0), 56.0);
        assert_eq!(x, -1280 + 520);
        assert!(y >= -200 && y + 64 <= -100, "{y}");
    }

    #[test]
    fn the_monitor_under_the_pointer_wins_else_the_first() {
        let monitors = [ONE_X, TWO_X];
        assert_eq!(monitor_under(Some((2500.0, 900.0)), &monitors), Some(TWO_X));
        assert_eq!(monitor_under(Some((10.0, 10.0)), &monitors), Some(ONE_X));
        assert_eq!(monitor_under(Some((-50.0, 10.0)), &monitors), Some(ONE_X));
        assert_eq!(monitor_under(None, &monitors), Some(ONE_X));
        assert_eq!(monitor_under(None, &[]), None);
    }
}
