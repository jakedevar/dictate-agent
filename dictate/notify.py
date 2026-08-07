"""Console-based notifier (no desktop notifications)."""

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

        # Instead of hitting notify-send/dunst, log to stdout so the daemon
        # still emits useful breadcrumbs when running interactively.
        line = f"[{self.app_name}] {title}"
        if message:
            line = f"{line}: {message}"
        print(line)

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
        """Show processing notification."""
        self.notify(
            f"Processing with {model}...",
            "Working on your request",
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
    """Notifications no longer depend on external tools."""
    return []
