"""Persistent floating status window for dictate agent state.

Uses overrideredirect=True to bypass i3's window management entirely.
The window lives at X11 screen level, so it appears on whichever
workspace is currently visible — no sticky/EWMH hacks required.
"""

import subprocess
import threading
import tkinter as tk
from enum import Enum
from typing import Optional


class WindowState(Enum):
    IDLE = "idle"
    RECORDING = "recording"
    TRANSCRIBING = "transcribing"
    CANCELLED = "cancelled"


_STATE_CONFIG = {
    WindowState.RECORDING: {
        "icon": "●",
        "label": "Recording",
        "bg": "#c0392b",
        "fg": "#ffffff",
    },
    WindowState.TRANSCRIBING: {
        "icon": "◐",
        "label": "Transcribing",
        "bg": "#d68910",
        "fg": "#ffffff",
    },
    WindowState.CANCELLED: {
        "icon": "✕",
        "label": "Cancelled",
        "bg": "#566573",
        "fg": "#ffffff",
    },
}

_CANCELLED_AUTO_HIDE_MS = 2000


class StatusWindow:
    """Floating status indicator showing dictate agent state.

    Runs in a daemon thread. Call start() once, then use
    recording() / transcribing() / cancelled() / hide() from any thread.
    """

    def __init__(
        self,
        position: str = "top-right",
        margin: int = 20,
        center_offset_y: int = 0,
        center_mode: bool = False,
    ):
        self.position = position
        self.margin = margin
        self.center_offset_y = center_offset_y
        self.center_mode = center_mode
        self._state = WindowState.IDLE
        self._root: Optional[tk.Tk] = None
        self._frame: Optional[tk.Frame] = None
        self._icon_label: Optional[tk.Label] = None
        self._text_label: Optional[tk.Label] = None
        self._hide_after_id: Optional[str] = None
        self._ready = threading.Event()

    def start(self) -> None:
        """Start the window thread. Blocks briefly until tkinter is initialized."""
        t = threading.Thread(target=self._run, daemon=True, name="dictate-status-window")
        t.start()
        self._ready.wait(timeout=3.0)

    def _run(self) -> None:
        """Tkinter main loop — runs in background thread."""
        try:
            root = tk.Tk()
        except Exception:
            # Tkinter unavailable (e.g. no DISPLAY) — degrade silently
            self._ready.set()
            return

        self._root = root

        # Remove all window decorations and bypass i3 window management.
        # With overrideredirect the window is unmanaged by i3 and lives at
        # X11 screen level: it's visible on whichever workspace is active.
        root.overrideredirect(True)
        root.wm_attributes("-topmost", True)
        root.withdraw()  # Start hidden

        # Outer frame with padding — acts as the visible pill/badge
        self._frame = tk.Frame(root, bg="#1e1e2e", padx=14, pady=8)
        self._frame.pack(fill=tk.BOTH, expand=True)

        self._icon_label = tk.Label(
            self._frame,
            text="",
            font=("monospace", 16),
            bg="#1e1e2e",
            fg="#ffffff",
        )
        self._icon_label.pack(side=tk.LEFT, padx=(0, 6))

        self._text_label = tk.Label(
            self._frame,
            text="",
            font=("Sans", 11, "bold"),
            bg="#1e1e2e",
            fg="#ffffff",
        )
        self._text_label.pack(side=tk.LEFT)

        self._ready.set()
        root.mainloop()

    # ------------------------------------------------------------------
    # Internal UI update (must run on the tkinter thread via after())
    # ------------------------------------------------------------------

    def _update_ui(self) -> None:
        """Apply current state to the window widgets."""
        root = self._root
        if root is None:
            return

        # Cancel any pending auto-hide
        if self._hide_after_id is not None:
            root.after_cancel(self._hide_after_id)
            self._hide_after_id = None

        if self._state == WindowState.IDLE:
            root.withdraw()
            return

        cfg = _STATE_CONFIG[self._state]
        bg = cfg["bg"]
        fg = cfg["fg"]

        self._frame.configure(bg=bg)
        self._icon_label.configure(text=cfg["icon"], bg=bg, fg=fg)
        self._text_label.configure(text=cfg["label"], bg=bg, fg=fg)

        # Compute geometry and position
        root.update_idletasks()
        w = self._frame.winfo_reqwidth()
        h = self._frame.winfo_reqheight()
        sw = root.winfo_screenwidth()
        sh = root.winfo_screenheight()
        x, y = self._compute_position(w, h, sw, sh)
        root.geometry(f"{w}x{h}+{x}+{y}")

        root.deiconify()
        root.lift()

        # Cancelled state auto-hides after a short delay
        if self._state == WindowState.CANCELLED:
            self._hide_after_id = root.after(
                _CANCELLED_AUTO_HIDE_MS,
                lambda: self._set_state_internal(WindowState.IDLE),
            )

    def _set_state_internal(self, state: WindowState) -> None:
        """Set state from within the tkinter thread (used by after() callbacks)."""
        self._state = state
        self._update_ui()

    def _compute_position(self, w: int, h: int, sw: int, sh: int) -> tuple[int, int]:
        m = self.margin
        if self.center_mode:
            cx = (sw - w) // 2
            cy = self.margin + self.center_offset_y
            return cx, cy
        if self.position == "top-left":
            return m, m
        elif self.position == "bottom-left":
            return m, sh - h - m
        elif self.position == "bottom-right":
            return sw - w - m, sh - h - m
        else:  # top-right (default)
            return sw - w - m, m

    # ------------------------------------------------------------------
    # Public API — thread-safe, callable from any thread
    # ------------------------------------------------------------------

    def set_state(self, state: WindowState) -> None:
        """Thread-safe state update."""
        if self._root is None:
            return
        self._state = state
        self._root.after(0, self._update_ui)

    def recording(self) -> None:
        """Show recording indicator."""
        self.set_state(WindowState.RECORDING)

    def transcribing(self) -> None:
        """Show transcribing indicator."""
        self.set_state(WindowState.TRANSCRIBING)

    def cancelled(self) -> None:
        """Show cancelled indicator (auto-hides after 2 s)."""
        self.set_state(WindowState.CANCELLED)

    def hide(self) -> None:
        """Hide the window (return to idle)."""
        self.set_state(WindowState.IDLE)


def check_status_window_dependencies() -> list[tuple[str, str]]:
    """Check that tkinter is available."""
    try:
        import tkinter  # noqa: F401
        return []
    except ImportError:
        return [("tkinter", "sudo pacman -S tk")]
