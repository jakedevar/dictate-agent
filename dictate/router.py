"""
Simplified routing based on first-word triggers.

Default is TYPE (just transcribe and type).
Keywords activate different backends:
- "Timer ..." -> Timer (systemd notification timer)
- "Simple ..." / "Easy ..." / "Medium ..." / "Hard ..." -> Local (Ollama qwen2.5:14b)
- "Edit: ..." / "Fix: ..." etc. -> Local edit mode
"""

from dataclasses import dataclass
from enum import Enum

from .config import RouterConfig


class RouteType(Enum):
    """Types of routes for transcribed text."""

    LOCAL = "local"  # Local model via Ollama
    EDIT = "edit"  # Text transformation mode
    COMMAND = "command"  # Keyboard/system command
    TIMER = "timer"  # Set a timer with notification
    TYPE = "type"  # Just type the text


@dataclass
class RouteResult:
    """Result of routing decision."""

    route: RouteType
    model: str  # Ollama model name if applicable
    text: str  # Processed text (trigger word stripped)
    confidence: float  # 0-1 confidence in routing decision


class Router:
    """Routes transcribed text to appropriate handler."""

    def __init__(self, config: RouterConfig):
        self.config = config

    def route(self, text: str) -> RouteResult:
        """
        Route based on first word trigger. Default is TYPE.

        Priority:
        1. Check for edit triggers (edit:, fix:, etc.)
        2. Check for AI triggers (simple, easy, medium, hard) -> all route to LOCAL
        3. Default to TYPE (just type the text)
        """
        text = text.strip()
        if not text:
            return RouteResult(RouteType.TYPE, "", text, 1.0)

        lower_text = text.lower()

        # Check for edit mode triggers
        for trigger in ["edit:", "fix:", "change:", "rewrite:", "transform:"]:
            if lower_text.startswith(trigger):
                return RouteResult(
                    route=RouteType.EDIT,
                    model="local",
                    text=text[len(trigger) :].strip(),
                    confidence=1.0,
                )

        # Check for AI triggers at start — all route to local Ollama
        words = text.split(maxsplit=1)
        first_word = words[0].lower().rstrip(".,!?:;")  # Strip trailing punctuation
        rest = words[1] if len(words) > 1 else ""

        if first_word == "timer":
            return RouteResult(RouteType.TIMER, "", rest, 1.0)
        elif first_word in ("simple", "easy", "medium", "hard"):
            return RouteResult(RouteType.LOCAL, "local", rest, 1.0)

        # Default: just type the text
        return RouteResult(RouteType.TYPE, "", text, 1.0)
