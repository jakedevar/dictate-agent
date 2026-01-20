"""
Output handling - typing text into active window via xdotool.
"""

import subprocess
import time
from dataclasses import dataclass
from typing import Optional


@dataclass
class OutputHandler:
    """Handles typing text into the active window."""

    typing_delay_ms: int = 10
    use_clipboard: bool = False
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
            # Move to end of text field first
            subprocess.run(
                ["xdotool", "key", "--clearmodifiers", "ctrl+End"],
                check=False,
            )
            time.sleep(0.02)

            # Get character before cursor for smart formatting
            char_before = self._get_char_before_cursor()

            # Format text based on context
            formatted_text = self._format_dictation(text, char_before)

            if self.use_clipboard:
                # Use clipboard method (faster for long text)
                self._type_via_clipboard(formatted_text)
            else:
                # Use xdotool type (more reliable)
                subprocess.run(
                    ["xdotool", "type", "--clearmodifiers", formatted_text],
                    check=True,
                )

            return True

        except Exception as e:
            print(f"Error typing text: {e}")
            return False

    def _get_char_before_cursor(self) -> str:
        """Get the character immediately before the cursor."""
        try:
            # Save current clipboard
            old_clip = subprocess.run(
                ["xclip", "-selection", "clipboard", "-o"],
                capture_output=True,
                text=True,
            ).stdout

            # Select character before cursor and copy it
            subprocess.run(
                ["xdotool", "key", "--clearmodifiers", "shift+Left"],
                check=True,
            )
            time.sleep(0.02)
            subprocess.run(
                ["xdotool", "key", "--clearmodifiers", "ctrl+c"],
                check=True,
            )
            time.sleep(0.02)
            # Move cursor back (deselect)
            subprocess.run(
                ["xdotool", "key", "--clearmodifiers", "Right"],
                check=True,
            )

            # Get the copied character
            result = subprocess.run(
                ["xclip", "-selection", "clipboard", "-o"],
                capture_output=True,
                text=True,
            )
            char = result.stdout

            # Restore original clipboard
            if old_clip:
                subprocess.run(
                    ["xclip", "-selection", "clipboard"],
                    input=old_clip,
                    text=True,
                )

            return char if len(char) == 1 else ""

        except Exception:
            return ""

    def _format_dictation(self, text: str, char_before: str) -> str:
        """Format dictated text based on context (spacing and capitalization)."""
        if not text:
            return text

        text = text.strip()
        if not text:
            return text

        # Characters that don't need a space before the new text
        no_space_before = set('(\'"[{<')
        # Characters that indicate sentence end (capitalize next word)
        sentence_end = set(".!?")
        # Whitespace characters
        whitespace = set(" \t\n\r")

        needs_space = False
        needs_capital = False

        if not char_before:
            # Empty/start of field - capitalize first letter
            needs_capital = True
        elif char_before in sentence_end:
            # After sentence-ending punctuation
            needs_space = True
            needs_capital = True
        elif char_before in whitespace:
            # Already have whitespace
            if char_before == "\n":
                needs_capital = True
        elif char_before in no_space_before:
            # After opening bracket/quote - no space, no capital
            pass
        else:
            # After any other character - add space
            needs_space = True

        # Apply capitalization
        if needs_capital and text:
            text = text[0].upper() + text[1:]

        # Apply spacing
        if needs_space:
            text = " " + text

        return text

    def _type_via_clipboard(self, text: str) -> None:
        """Type text using clipboard paste (faster for long text)."""
        # Save current clipboard
        old_clip = subprocess.run(
            ["xclip", "-selection", "clipboard", "-o"],
            capture_output=True,
            text=True,
        ).stdout

        # Set clipboard to our text
        subprocess.run(
            ["xclip", "-selection", "clipboard"],
            input=text,
            text=True,
            check=True,
        )

        # Paste
        subprocess.run(
            ["xdotool", "key", "--clearmodifiers", "ctrl+v"],
            check=True,
        )

        time.sleep(0.05)

        # Restore original clipboard
        if old_clip:
            subprocess.run(
                ["xclip", "-selection", "clipboard"],
                input=old_clip,
                text=True,
            )


def check_output_dependencies() -> list[tuple[str, str]]:
    """Check output dependencies. Returns list of (cmd, package) missing."""
    missing = []

    for cmd, pkg in [("xdotool", "xdotool"), ("xclip", "xclip")]:
        result = subprocess.run(["which", cmd], capture_output=True)
        if result.returncode != 0:
            missing.append((cmd, pkg))

    return missing
