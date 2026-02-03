"""
Timer executor using systemd-run for persistent, notification-based timers.

Usage: Say "timer 5 minutes" or "timer 30 seconds check the oven"
The duration is parsed and a systemd transient timer is created that
fires a desktop notification (and optional sound) when it expires.
"""

import re
import subprocess
from dataclasses import dataclass
from typing import Optional

# Word-form numbers for Whisper transcription output
WORD_TO_NUM = {
    "zero": 0, "one": 1, "two": 2, "three": 3, "four": 4,
    "five": 5, "six": 6, "seven": 7, "eight": 8, "nine": 9,
    "ten": 10, "eleven": 11, "twelve": 12, "thirteen": 13,
    "fourteen": 14, "fifteen": 15, "sixteen": 16, "seventeen": 17,
    "eighteen": 18, "nineteen": 19, "twenty": 20, "thirty": 30,
    "forty": 40, "fifty": 50, "sixty": 60, "ninety": 90,
    "a": 1, "an": 1,
}

UNIT_ALIASES = {
    "second": "s", "seconds": "s", "sec": "s", "secs": "s", "s": "s",
    "minute": "m", "minutes": "m", "min": "m", "mins": "m", "m": "m",
    "hour": "h", "hours": "h", "hr": "h", "hrs": "h", "h": "h",
}


@dataclass
class TimerResult:
    """Result from setting a timer."""

    success: bool
    response: str
    error: Optional[str] = None


def _parse_word_number(word: str) -> Optional[int]:
    """Convert a word-form number to int, or parse a digit string."""
    word = word.lower().strip()
    if word.isdigit():
        return int(word)
    return WORD_TO_NUM.get(word)


def parse_duration(text: str) -> tuple[Optional[int], str]:
    """
    Parse a natural language duration from text.

    Returns (total_seconds, remaining_text) or (None, original_text) on failure.

    Handles:
      - "5 minutes"
      - "five minutes"
      - "1 hour 30 minutes"
      - "a minute"
      - "90 seconds"
      - "2 and a half minutes"
      - "half hour" / "half an hour"
    """
    text = text.strip()
    if not text:
        return None, text

    original = text
    total_seconds = 0
    found_any = False

    # Handle "half hour" / "half an hour" at the start
    half_hour_match = re.match(r"half\s+(?:an?\s+)?hour", text, re.IGNORECASE)
    if half_hour_match:
        total_seconds += 1800
        text = text[half_hour_match.end():].strip()
        found_any = True

    # Pattern: <number> [and a half] <unit>
    # Sort alternatives longest-first so "minutes" matches before "minute", etc.
    num_alts = sorted(WORD_TO_NUM.keys(), key=len, reverse=True)
    unit_alts = sorted(UNIT_ALIASES.keys(), key=len, reverse=True)
    pattern = re.compile(
        r"(\d+|" + "|".join(re.escape(w) for w in num_alts) + r")"
        r"(?:\s+and\s+a\s+half)?"
        r"\s+"
        r"(" + "|".join(re.escape(u) for u in unit_alts) + r")"
        r"(?=\s|$)",  # must be followed by space or end-of-string
        re.IGNORECASE,
    )

    # Try matching duration at current position, consume and repeat
    while True:
        match = pattern.match(text)
        if not match:
            break

        num = _parse_word_number(match.group(1))
        if num is None:
            break

        unit = UNIT_ALIASES.get(match.group(2).lower())
        if unit is None:
            break

        has_half = "and a half" in match.group(0).lower()

        if unit == "s":
            total_seconds += num + (30 if has_half else 0)
        elif unit == "m":
            total_seconds += num * 60 + (30 if has_half else 0)
        elif unit == "h":
            total_seconds += num * 3600 + (1800 if has_half else 0)

        found_any = True
        text = text[match.end():].strip()

    # Fallback: search for duration anywhere in text (handles filler words)
    if not found_any:
        match = pattern.search(original)
        if match:
            num = _parse_word_number(match.group(1))
            unit = UNIT_ALIASES.get(match.group(2).lower())
            if num is not None and unit is not None:
                has_half = "and a half" in match.group(0).lower()
                if unit == "s":
                    total_seconds += num + (30 if has_half else 0)
                elif unit == "m":
                    total_seconds += num * 60 + (30 if has_half else 0)
                elif unit == "h":
                    total_seconds += num * 3600 + (1800 if has_half else 0)
                found_any = True
                text = original[match.end():].strip()

    if not found_any:
        return None, original

    return total_seconds, text


def _format_duration_human(seconds: int) -> str:
    """Format seconds into a human-readable string."""
    parts = []
    if seconds >= 3600:
        h = seconds // 3600
        parts.append(f"{h} hour{'s' if h != 1 else ''}")
        seconds %= 3600
    if seconds >= 60:
        m = seconds // 60
        parts.append(f"{m} minute{'s' if m != 1 else ''}")
        seconds %= 60
    if seconds > 0 or not parts:
        parts.append(f"{seconds} second{'s' if seconds != 1 else ''}")
    return " ".join(parts)


def _format_duration_systemd(seconds: int) -> str:
    """Format seconds into systemd OnActiveSec format (e.g. '5m', '1h30m')."""
    parts = []
    if seconds >= 3600:
        parts.append(f"{seconds // 3600}h")
        seconds %= 3600
    if seconds >= 60:
        parts.append(f"{seconds // 60}m")
        seconds %= 60
    if seconds > 0:
        parts.append(f"{seconds}s")
    return "".join(parts) or "0s"


class TimerExecutor:
    """Sets timers using systemd-run transient timer units."""

    def __init__(self, sound_enabled: bool = True):
        self.sound_enabled = sound_enabled

    def execute(self, text: str) -> TimerResult:
        """
        Parse a timer request and create a systemd transient timer.

        Args:
            text: The text after "timer" keyword is stripped, e.g. "5 minutes check the oven"

        Returns:
            TimerResult with confirmation or error
        """
        seconds, label = parse_duration(text)

        if seconds is None or seconds <= 0:
            return TimerResult(
                success=False,
                response="",
                error=f"Could not parse timer duration from: {text}",
            )

        # Use remaining text as label, or a default
        label = label.strip(" .,!?") if label else ""
        display_label = label if label else "Timer complete"
        human_duration = _format_duration_human(seconds)
        systemd_duration = _format_duration_systemd(seconds)

        # Build the notification command that fires when the timer expires
        notify_cmd = (
            f'notify-send -a "Dictate Agent" -i alarm-symbolic '
            f'-u critical '
            f'"Timer: {display_label}" '
            f'"{human_duration} elapsed"'
        )

        # Optionally play a sound
        if self.sound_enabled:
            notify_cmd += (
                " ; paplay /usr/share/sounds/freedesktop/stereo/complete.oga 2>/dev/null"
                " || true"
            )

        try:
            result = subprocess.run(
                [
                    "systemd-run",
                    "--user",
                    "--on-active=" + systemd_duration,
                    "--description=Dictate Agent Timer",
                    "/bin/bash", "-c", notify_cmd,
                ],
                capture_output=True,
                text=True,
                timeout=5,
            )

            if result.returncode != 0:
                error = result.stderr.strip() or "systemd-run failed"
                print(f"Timer error: {error}")
                return TimerResult(
                    success=False,
                    response="",
                    error=f"Failed to set timer: {error}",
                )

            msg = f"Timer set for {human_duration}"
            if label:
                msg += f": {label}"
            print(f"Timer created: {systemd_duration} ({display_label})")
            return TimerResult(success=True, response=msg)

        except FileNotFoundError:
            return TimerResult(
                success=False,
                response="",
                error="systemd-run not found. Is systemd available?",
            )
        except subprocess.TimeoutExpired:
            return TimerResult(
                success=False,
                response="",
                error="Timed out creating timer",
            )
        except Exception as e:
            return TimerResult(
                success=False,
                response="",
                error=f"Timer error: {e}",
            )
