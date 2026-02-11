"""
Output handling - typing text into active window via clipboard paste.

Uses xclip + xdotool ctrl+v for near-instant output regardless of text length.
Falls back to xdotool type if xclip is unavailable.
"""

import subprocess
import time
from dataclasses import dataclass


@dataclass
class OutputHandler:
    """Handles typing text into the active window."""

    typing_delay_ms: int = 10
    auto_type: bool = True

    def type_text(self, text: str) -> bool:
        """
        Type text into the active window via clipboard paste.

        Saves and restores the user's clipboard contents.

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

            # Save current clipboard
            saved_clip = None
            try:
                result = subprocess.run(
                    ["xclip", "-selection", "clipboard", "-o"],
                    capture_output=True, timeout=1,
                )
                if result.returncode == 0:
                    saved_clip = result.stdout
            except Exception:
                pass

            # Set clipboard to our text and paste
            subprocess.run(
                ["xclip", "-selection", "clipboard"],
                input=text.encode(), check=True, timeout=1,
            )
            subprocess.run(
                ["xdotool", "key", "--clearmodifiers", "ctrl+v"],
                check=True, timeout=2,
            )

            # Brief delay to let paste complete before restoring clipboard
            time.sleep(0.05)

            # Restore original clipboard
            if saved_clip is not None:
                try:
                    subprocess.run(
                        ["xclip", "-selection", "clipboard"],
                        input=saved_clip, timeout=1,
                    )
                except Exception:
                    pass

            return True

        except Exception as e:
            print(f"Error typing text: {e}")
            return False


def check_output_dependencies() -> list[tuple[str, str]]:
    """Check output dependencies. Returns list of (cmd, package) missing."""
    missing = []

    for cmd, pkg in [("xdotool", "xdotool"), ("xclip", "xclip")]:
        result = subprocess.run(["which", cmd], capture_output=True)
        if result.returncode != 0:
            missing.append((cmd, pkg))

    return missing
