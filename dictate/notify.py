"""Desktop notifications via notify-send."""

import subprocess
from dataclasses import dataclass


@dataclass
class Notifier:
    """Sends desktop notifications."""

    enabled: bool = True
    default_timeout_ms: int = 3000
    app_name: str = "Dictate Agent"

    def notify(
        self,
        title: str,
        message: str = "",
        icon: str = "dialog-information",
        timeout_ms: int = 0,
        replace: bool = True,
    ) -> None:
        """
        Send a desktop notification.

        Args:
            title: Notification title
            message: Notification body
            icon: Icon name
            timeout_ms: How long to show (0 = use default)
            replace: Replace previous notification with same tag
        """
        if not self.enabled:
            return

        timeout = timeout_ms or self.default_timeout_ms

        cmd = [
            "notify-send",
            "-a", self.app_name,
            "-i", icon,
            "-t", str(timeout),
        ]

        # Use synchronous hint to replace previous notifications
        if replace:
            cmd.extend(["-h", "string:x-canonical-private-synchronous:dictate-agent"])

        cmd.extend([title, message])

        try:
            subprocess.run(cmd, capture_output=True, check=False)
        except Exception:
            pass  # Don't let notification failures break the app

    def recording(self) -> None:
        """Show recording notification."""
        self.notify(
            "Recording...",
            "Toggle again to stop",
            "audio-input-microphone",
            30000,
        )

    def transcribing(self) -> None:
        """Show transcribing notification."""
        self.notify(
            "Transcribing...",
            "Processing speech",
            "emblem-synchronizing",
            30000,
        )

    def processing(self, model: str) -> None:
        """Show processing with Claude notification."""
        self.notify(
            f"Processing with {model}...",
            "Waiting for Claude",
            "emblem-synchronizing",
            30000,
        )

    def done(self, text: str) -> None:
        """Show completion notification."""
        # Truncate long text
        display = text[:100] + "..." if len(text) > 100 else text
        self.notify(
            "Done!",
            display,
            "emblem-ok-symbolic",
            3000,
        )

    def error(self, message: str) -> None:
        """Show error notification."""
        self.notify(
            "Error",
            message[:100],
            "dialog-error",
            5000,
        )

    def no_speech(self) -> None:
        """Show no speech detected notification."""
        self.notify(
            "No speech detected",
            "Try speaking louder",
            "dialog-warning",
            2000,
        )

    def not_running(self) -> None:
        """Show daemon not running notification."""
        self.notify(
            "Dictate not running",
            "Start with: dictate-agent",
            "dialog-warning",
            3000,
        )


def check_notify_dependencies() -> list[tuple[str, str]]:
    """Check notification dependencies."""
    missing = []

    result = subprocess.run(["which", "notify-send"], capture_output=True)
    if result.returncode != 0:
        missing.append(("notify-send", "libnotify"))

    return missing
