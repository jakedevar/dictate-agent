//! Making the Flow bar a well-behaved X11 overlay.
//!
//! The one property that matters above all: **the bar never takes keyboard
//! focus**. The daemon pastes into whatever window has focus, so a bar that
//! grabs it on show would receive the user's own dictation. Everything here is
//! verified end to end by `ui/tests/x11/hud-focus.sh` (i3 inside Xvfb).
//!
//! | Property | Mechanism | Why this one |
//! |---|---|---|
//! | never focused by the WM | `WM_HINTS.input = False` (`set_accept_focus(false)`) | ICCCM "no input" model; i3 then never calls `XSetInputFocus` on it |
//! | not focused on map | `_NET_WM_USER_TIME = 0` (`set_focus_on_map(false)`) | EWMH focus-stealing prevention; i3 skips focusing a window mapped with user time 0 |
//! | floats in i3 | `_NET_WM_WINDOW_TYPE_NOTIFICATION` | i3 floats notification windows automatically, no user rule needed |
//! | above, on every workspace, off taskbars | `_NET_WM_STATE_ABOVE`, `_STICKY`, `_SKIP_TASKBAR`, `_SKIP_PAGER` | |
//! | click-through | an **empty** XShape input region on the toplevel | pointer events (and focus-follows-mouse `EnterNotify`) go to the window underneath |
//! | no black box without a compositor | a rounded XShape *bounding* region matching the pill | only when the screen is not composited |
//!
//! ## Why not Tauri's own switches
//!
//! - `focused(false)` alone is not enough: tao re-enables `accept_focus` after
//!   the first draw when the window is "focusable", so the bar is built with
//!   `focusable(false)` as well, and the hints are re-asserted here.
//! - `set_ignore_cursor_events(true)` in tao 0.37 sets a 1×1 input rectangle
//!   (a clickable pixel at the corner) on the `GdkWindow` directly, which GTK
//!   drops if the widget is re-realized. The region here is truly empty and is
//!   set on the widget, which GTK re-applies on every realize.
//!
//! ## If a window manager ignores the hints
//!
//! i3 honors all of them. For any other manager, or an i3 config that
//! overrides them, the fallback is an explicit rule (documented in
//! `ui/README.md`):
//!
//! ```text
//! for_window [class="dictate-ui" title="^Flow bar$"] floating enable, sticky enable, border none
//! no_focus [class="dictate-ui" title="^Flow bar$"]
//! ```

use gtk::prelude::*;

/// The pill's geometry inside the bar window, in logical pixels. Mirrored by
/// `ui/src/hud/hud.css` (`--pill-*`); keep them in step.
pub const PILL_INSET: i32 = 8;
/// See [`PILL_INSET`].
pub const PILL_RADIUS: i32 = 20;

/// Apply every X11 property above to the bar. Call before its first show.
///
/// # Errors
///
/// If the window has no GTK backing (not running on GTK).
pub fn harden_overlay(window: &tauri::WebviewWindow, size: (u32, u32)) -> Result<(), String> {
    let gtk_window = window.gtk_window().map_err(|e| e.to_string())?;
    gtk_window.set_type_hint(gtk::gdk::WindowTypeHint::Notification);
    gtk_window.set_accept_focus(false);
    gtk_window.set_focus_on_map(false);
    gtk_window.set_keep_above(true);
    gtk_window.set_skip_taskbar_hint(true);
    gtk_window.set_skip_pager_hint(true);
    gtk_window.set_decorated(false);
    gtk_window.set_resizable(false);
    gtk_window.stick();

    // Click-through: an empty input region.
    gtk_window.input_shape_combine_region(Some(&gtk::cairo::Region::create()));

    // Without a compositor the transparent corners would paint as an opaque
    // box; cut the window itself to the pill instead.
    let composited = gtk::prelude::WidgetExt::screen(&gtk_window)
        .is_some_and(|s| s.is_composited());
    if !composited {
        let (w, h) = (size.0 as i32, size.1 as i32);
        gtk_window.shape_combine_region(Some(&rounded_region(
            PILL_INSET,
            PILL_INSET,
            w - 2 * PILL_INSET,
            h - 2 * PILL_INSET,
            PILL_RADIUS,
        )));
    }
    Ok(())
}

/// A rounded rectangle as a union of one-pixel-high spans.
fn rounded_region(x: i32, y: i32, w: i32, h: i32, r: i32) -> gtk::cairo::Region {
    let region = gtk::cairo::Region::create();
    for (dx, dy, width) in rounded_spans(w, h, r) {
        let _ = region.union_rectangle(&gtk::cairo::RectangleInt::new(x + dx, y + dy, width, 1));
    }
    region
}

/// `(x offset, row, width)` for each row of a `w`×`h` rounded rectangle.
fn rounded_spans(w: i32, h: i32, r: i32) -> Vec<(i32, i32, i32)> {
    let r = r.min(w / 2).min(h / 2).max(0);
    (0..h)
        .map(|row| {
            // Distance from the row's centre to the nearest corner-circle centre.
            let dy = if row < r {
                f64::from(r - row) - 0.5
            } else if row >= h - r {
                f64::from(row - (h - r)) + 0.5
            } else {
                0.0
            };
            let inset = if dy > 0.0 {
                let rr = f64::from(r);
                (rr - (rr * rr - dy * dy).max(0.0).sqrt()).round() as i32
            } else {
                0
            };
            (inset, row, (w - 2 * inset).max(0))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::rounded_spans;

    #[test]
    fn spans_are_symmetric_with_rounded_ends_and_a_full_middle() {
        let spans = rounded_spans(216, 40, 20);
        assert_eq!(spans.len(), 40);
        let (x0, _, w0) = spans[0];
        assert!(x0 > 10 && w0 < 200, "top row is inset: {x0}");
        assert_eq!(spans[20], (0, 20, 216), "the middle row is full width");
        for i in 0..20 {
            assert_eq!(spans[i].0, spans[39 - i].0, "row {i} mirrors");
        }
        assert!(spans.windows(2).take(19).all(|p| p[1].0 <= p[0].0), "monotone");
    }

    #[test]
    fn a_radius_larger_than_the_box_is_clamped() {
        let spans = rounded_spans(10, 4, 50);
        assert!(spans.iter().all(|&(x, _, w)| x >= 0 && w >= 0 && x + w <= 10));
    }
}
