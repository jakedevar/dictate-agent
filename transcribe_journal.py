"""
One-off script to transcribe Jake's voice journal recording and save to markdown.
"""

import subprocess
import sys
import tempfile
from datetime import datetime
from pathlib import Path

# Add project to path
sys.path.insert(0, str(Path(__file__).parent))

INPUT_FILE = Path("/home/jakedevar/Downloads/New Recording 800.m4a")
OUTPUT_FILE = Path("/home/jakedevar/Downloads/negotiation_offer_voice_journal.md")


def convert_to_wav(input_path: Path, wav_path: Path) -> bool:
    """Convert m4a to 16kHz mono WAV using ffmpeg."""
    print(f"Converting {input_path.name} to WAV (16kHz mono)...")
    result = subprocess.run(
        [
            "ffmpeg", "-y",
            "-i", str(input_path),
            "-ar", "16000",
            "-ac", "1",
            "-f", "wav",
            str(wav_path),
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(f"ffmpeg error: {result.stderr}")
        return False
    print("Conversion done.")
    return True


def main():
    if not INPUT_FILE.exists():
        print(f"ERROR: Input file not found: {INPUT_FILE}")
        sys.exit(1)

    from dictate.config import WhisperConfig
    from dictate.transcribe import Transcriber

    config = WhisperConfig()

    print(f"Loading Whisper model: {config.model}")
    transcriber = Transcriber(config)
    transcriber._load_models()  # Load synchronously

    if transcriber.model_error:
        print(f"ERROR loading model: {transcriber.model_error}")
        sys.exit(1)

    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
        wav_path = Path(tmp.name)

    try:
        if not convert_to_wav(INPUT_FILE, wav_path):
            sys.exit(1)

        print("Transcribing... (this may take a few minutes for a long recording)")
        result = transcriber.transcribe(wav_path)

        if not result:
            print("ERROR: Transcription returned no result.")
            sys.exit(1)

        print(f"\nTranscription complete! ({result.duration_s:.1f}s of audio)")
        print(f"Text length: {len(result.text)} characters")

        # Write markdown file
        now = datetime.now().strftime("%Y-%m-%d %H:%M")
        md_content = f"""# Jake's Trauma Voice Journal

*Transcribed: {now}*
*Source: {INPUT_FILE.name}*
*Audio duration: {result.duration_s:.1f}s*

---

{result.text}
"""
        OUTPUT_FILE.write_text(md_content, encoding="utf-8")
        print(f"\nSaved to: {OUTPUT_FILE}")

    finally:
        wav_path.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
