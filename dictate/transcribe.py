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
    AutoModelForCausalLM,
    AutoModelForSpeechSeq2Seq,
    AutoProcessor,
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

    Uses HuggingFace Transformers with direct model.generate() calls for
    proper speculative decoding support (not pipeline which has issues).
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

        self.model = None
        self.processor = None
        self.assistant_model = None
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
        """Internal: Load Whisper model and optional assistant for speculative decoding."""
        try:
            print(f"Loading Whisper model: {self.config.model}")
            print(f"Device: {self.device}, dtype: {self.torch_dtype}")

            # Load main model with SDPA attention (Blackwell compatible)
            self.model = AutoModelForSpeechSeq2Seq.from_pretrained(
                self.config.model,
                dtype=self.torch_dtype,
                low_cpu_mem_usage=True,
                use_safetensors=True,
                attn_implementation="sdpa",
            )
            self.model.to(self.device)

            self.processor = AutoProcessor.from_pretrained(self.config.model)

            # Load assistant model for speculative decoding (2x additional speedup)
            if self.config.use_speculative_decoding:
                print(f"Loading assistant model: {self.config.assistant_model}")
                self.assistant_model = AutoModelForCausalLM.from_pretrained(
                    self.config.assistant_model,
                    dtype=self.torch_dtype,
                    low_cpu_mem_usage=True,
                    use_safetensors=True,
                    attn_implementation="sdpa",
                )
                self.assistant_model.to(self.device)
                print("Speculative decoding enabled (2x speedup)")

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
        Transcribe audio file using direct model.generate() for proper
        speculative decoding support.

        Args:
            audio_path: Path to WAV file (16kHz mono)

        Returns:
            TranscriptionResult or None on error
        """
        # Wait for model to be loaded
        self.model_loaded.wait()

        if self.model_error:
            print(f"Cannot transcribe: {self.model_error}")
            return None

        if not self.model or not self.processor:
            print("Model not initialized")
            return None

        try:
            import librosa
        except ImportError:
            # Fallback to torchaudio if librosa not available
            import torchaudio

        try:
            # Load audio - try librosa first, fall back to torchaudio
            try:
                import librosa
                audio, sr = librosa.load(str(audio_path), sr=16000)
            except ImportError:
                import torchaudio
                audio, sr = torchaudio.load(str(audio_path))
                if sr != 16000:
                    resampler = torchaudio.transforms.Resample(sr, 16000)
                    audio = resampler(audio)
                audio = audio.squeeze().numpy()

            # Process audio
            inputs = self.processor(
                audio,
                sampling_rate=16000,
                return_tensors="pt",
            )
            input_features = inputs.input_features.to(self.device, dtype=self.torch_dtype)

            # Generate with or without speculative decoding
            generate_kwargs = {
                "input_features": input_features,
                "language": "en",
                "task": "transcribe",
                "return_timestamps": False,
            }

            if self.assistant_model is not None:
                generate_kwargs["assistant_model"] = self.assistant_model

            with torch.no_grad():
                predicted_ids = self.model.generate(**generate_kwargs)

            # Decode
            text = self.processor.batch_decode(
                predicted_ids,
                skip_special_tokens=True,
            )[0].strip()

            # Apply common corrections
            text = self._apply_corrections(text)

            return TranscriptionResult(
                text=text,
                language="en",
                duration_s=len(audio) / 16000,
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
