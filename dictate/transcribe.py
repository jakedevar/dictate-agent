"""
Optimized Whisper transcription using HuggingFace Transformers.

Features:
- large-v3-turbo model (6-8x faster than large-v3)
- Speculative decoding with distil-whisper (additional 2x speedup)
- SDPA attention (works on Blackwell/RTX 5080)
- Pre-loaded models for instant inference
"""

import threading
import warnings
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

import torch

# Suppress transformers deprecation warnings
warnings.filterwarnings("ignore", message=".*chunk_length_s.*is very experimental.*")
warnings.filterwarnings("ignore", message=".*forced_decoder_ids.*deprecated.*")

from transformers import (
    AutoModelForSpeechSeq2Seq,
    AutoProcessor,
    pipeline as hf_pipeline,
)

from .config import WhisperConfig


@dataclass
class TranscriptionResult:
    """Result of transcription."""

    text: str
    language: str
    duration_s: float


class Transcriber:
    """
    Optimized Whisper transcriber with speculative decoding.

    Uses HuggingFace pipeline with chunk_length_s for automatic long-form
    audio support. Audio >30s is split into overlapping chunks, each
    transcribed independently and merged.
    """

    def __init__(
        self,
        config: WhisperConfig,
        on_ready: Optional[Callable[[], None]] = None,
        on_error: Optional[Callable[[str], None]] = None,
    ):
        self.config = config
        self.on_ready = on_ready
        self.on_error = on_error

        self.pipe = None
        self.model_loaded = threading.Event()
        self.model_error: Optional[str] = None

        # Determine device and dtype
        self.device = "cuda:0" if config.device == "cuda" and torch.cuda.is_available() else "cpu"
        self.torch_dtype = torch.float16 if self.device.startswith("cuda") else torch.float32

    def load_models_async(self) -> None:
        """Load models in background thread."""
        thread = threading.Thread(target=self._load_models, daemon=True)
        thread.start()

    def _load_models(self) -> None:
        """Internal: Load Whisper model and build pipeline with chunking support."""
        try:
            print(f"Loading Whisper model: {self.config.model}")
            print(f"Device: {self.device}, dtype: {self.torch_dtype}")

            # Load main model with SDPA attention (Blackwell compatible)
            model = AutoModelForSpeechSeq2Seq.from_pretrained(
                self.config.model,
                dtype=self.torch_dtype,
                low_cpu_mem_usage=True,
                use_safetensors=True,
                attn_implementation="sdpa",
            )
            model.to(self.device)

            processor = AutoProcessor.from_pretrained(self.config.model)

            generate_kwargs = {
                "language": "en",
                "task": "transcribe",
                "return_timestamps": True,
                "max_new_tokens": 128,
                "no_repeat_ngram_size": 3,
            }

            # Note: speculative decoding (assistant_model) is incompatible with
            # Whisper's chunked pipeline — its custom generate_with_fallback path
            # doesn't route assistant_model to assisted generation, causing it to
            # leak into forward(). Chunked long-form support is the better tradeoff.
            if self.config.use_speculative_decoding:
                print("Speculative decoding skipped (incompatible with chunked pipeline)")

            # Pipeline handles chunking automatically for audio >30s
            self.pipe = hf_pipeline(
                "automatic-speech-recognition",
                model=model,
                tokenizer=processor.tokenizer,
                feature_extractor=processor.feature_extractor,
                dtype=self.torch_dtype,
                device=self.device,
                chunk_length_s=self.config.chunk_length_s,
                batch_size=self.config.batch_size,
                generate_kwargs=generate_kwargs,
            )

            self.model_loaded.set()
            print("Models loaded. Ready for transcription!")

            if self.on_ready:
                self.on_ready()

        except Exception as e:
            self.model_error = str(e)
            self.model_loaded.set()
            print(f"Failed to load model: {e}")

            if self.on_error:
                self.on_error(str(e))

    def transcribe(self, audio_path: Path) -> Optional[TranscriptionResult]:
        """
        Transcribe audio file using HuggingFace pipeline with automatic
        chunking for long-form audio support.

        Args:
            audio_path: Path to WAV file (16kHz mono)

        Returns:
            TranscriptionResult or None on error
        """
        self.model_loaded.wait()

        if self.model_error:
            print(f"Cannot transcribe: {self.model_error}")
            return None

        if not self.pipe:
            print("Pipeline not initialized")
            return None

        try:
            import time as _time

            # Compute duration from WAV file (16kHz, 16-bit mono = 32000 bytes/sec)
            file_size = audio_path.stat().st_size
            duration_s = max(0, (file_size - 44)) / (16000 * 2)
            print(f"[timing] Audio: {duration_s:.1f}s ({file_size} bytes)")

            # Pipeline chunks audio automatically for recordings >30s
            t0 = _time.monotonic()
            result = self.pipe(str(audio_path))
            t1 = _time.monotonic()
            print(f"[timing] Transcription: {t1 - t0:.2f}s")
            text = result["text"].strip()

            if not text:
                return None

            text = self._apply_corrections(text)

            return TranscriptionResult(
                text=text,
                language="en",
                duration_s=duration_s,
            )

        except Exception as e:
            print(f"Transcription error: {e}")
            import traceback
            traceback.print_exc()
            return None

    def _apply_corrections(self, text: str) -> str:
        """Apply common transcription corrections."""
        # Claude name corrections
        corrections = [
            (".clod", ".claude"),
            (".cloud", ".claude"),
            (".clawed", ".claude"),
            (" clod", " claude"),
            (" cloud", " claude"),
            (" clawed", " claude"),
            ("Clod", "Claude"),
            ("Cloud", "Claude"),
            ("Clawed", "Claude"),
            # Slash command corrections
            ("research code base", "/research_codebase"),
            ("Research code base", "/research_codebase"),
            ("research codebase", "/research_codebase"),
            ("Research codebase", "/research_codebase"),
            ("create plan", "/create_plan"),
            ("Create plan", "/create_plan"),
            ("implement plan", "/implement_plan"),
            ("Implement plan", "/implement_plan"),
            ("validate plan", "/validate_plan"),
            ("Validate plan", "/validate_plan"),
            ("create handoff", "/create_handoff"),
            ("Create handoff", "/create_handoff"),
            ("create hand off", "/create_handoff"),
            ("Create hand off", "/create_handoff"),
        ]

        for old, new in corrections:
            text = text.replace(old, new)

        return text

    def is_ready(self) -> bool:
        """Check if transcriber is ready."""
        return self.model_loaded.is_set() and self.model_error is None


def check_transcription_dependencies() -> list[tuple[str, str]]:
    """Check transcription dependencies. Returns list of (dep, instruction) missing."""
    missing = []

    try:
        import torch

        if not torch.cuda.is_available():
            missing.append(("CUDA", "Install PyTorch with CUDA support"))
    except ImportError:
        missing.append(("torch", "pip install torch"))

    try:
        import transformers
    except ImportError:
        missing.append(("transformers", "pip install transformers"))

    return missing
