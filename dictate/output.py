"""
Output handling - typing text into active window via xdotool.
"""

import subprocess
from dataclasses import dataclass


@dataclass
class OutputHandler:
    """Handles typing text into the active window."""

    typing_delay_ms: int = 10
    auto_type: bool = True

    def type_text(self, text: str) -> bool:
        """
        Type text into the active window.

        Args:
            text: Text to type

        Returns:
            True if successful
        """
        if not text or not self.auto_type:
            return False

        try:
            text = text.strip()
            if not text:
                return False

            subprocess.run(
                ["xdotool", "type", "--clearmodifiers", text],
                check=True,
            )

            return True

        except Exception as e:
            print(f"Error typing text: {e}")
            return False


def check_output_dependencies() -> list[tuple[str, str]]:
    """Check output dependencies. Returns list of (cmd, package) missing."""
    missing = []

    for cmd, pkg in [("xdotool", "xdotool")]:
        result = subprocess.run(["which", cmd], capture_output=True)
        if result.returncode != 0:
            missing.append((cmd, pkg))

    return missing
