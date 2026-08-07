"""Audio capture using parecord (PipeWire/PulseAudio)."""

import subprocess
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional


@dataclass
class AudioCapture:
    """Manages audio recording via parecord."""

    temp_file: Optional[Path] = None
    process: Optional[subprocess.Popen] = None
    is_recording: bool = False

    def start(self) -> Path:
        """Start recording audio. Returns path to temp WAV file."""
        if self.is_recording:
            raise RuntimeError("Already recording")

        # Create temp file for recording
        fd = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
        fd.close()
        self.temp_file = Path(fd.name)

        # Flush any stale audio in the buffer
        flush = subprocess.Popen(
            ["parecord", "--raw", "-n", "1"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        time.sleep(0.05)
        flush.terminate()
        flush.wait()

        # Start recording at 16kHz mono (what Whisper expects)
        # Use larger buffer (200ms) to prevent dropouts on longer recordings
        self.process = subprocess.Popen(
            [
                "parecord",
                "--format=s16le",
                "--rate=16000",
                "--channels=1",
                "--file-format=wav",
                "--latency-msec=200",
                str(self.temp_file),
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        self.is_recording = True
        return self.temp_file

    def stop(self) -> Optional[Path]:
        """Stop recording and return path to the audio file."""
        if not self.is_recording or not self.process:
            return None

        # Brief delay to capture trailing audio (prevents cutoff at end of speech)
        time.sleep(0.5)

        # Send SIGINT for clean shutdown
        import signal

        self.process.send_signal(signal.SIGINT)
        self.process.wait()
        self.process = None
        self.is_recording = False

        # Small delay to ensure file is fully written
        time.sleep(0.1)

        return self.temp_file

    def cleanup(self) -> None:
        """Remove temporary audio file."""
        if self.temp_file and self.temp_file.exists():
            self.temp_file.unlink()
            self.temp_file = None


def check_audio_dependencies() -> list[tuple[str, str]]:
    """Check that audio dependencies are available. Returns list of (cmd, package) missing."""
    missing = []
    for cmd, pkg in [("parecord", "pulseaudio-utils")]:
        result = subprocess.run(["which", cmd], capture_output=True)
        if result.returncode != 0:
            missing.append((cmd, pkg))
    return missing
