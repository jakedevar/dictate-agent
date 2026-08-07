"""
Grammar correction pipeline middleware using Ollama.

Fail-open design: on ANY error, returns original text unchanged.
Runs between transcription and routing in _stop_recording().
"""

import time
from dataclasses import dataclass
from typing import Optional


GRAMMAR_PROMPT = """\
Fix only grammar, spelling, and punctuation errors in the following text. \
Do not change meaning, add words, remove words, or rephrase. \
Output ONLY the corrected text with no explanation.

Text: {text}"""


@dataclass
class GrammarResult:
    """Result from grammar correction."""

    success: bool
    corrected: str  # Always safe to use (original on failure)
    original: str
    duration_s: float = 0.0
    error: Optional[str] = None


class GrammarCorrector:
    """Grammar correction via Ollama. Fail-open middleware."""

    def __init__(
        self,
        host: str = "http://localhost:11434",
        model: str = "qwen3:0.6b",
        timeout_s: float = 10.0,
        enabled: bool = True,
        min_words: int = 3,
    ):
        self.host = host
        self.model = model
        self.timeout = timeout_s
        self.enabled = enabled
        self.min_words = min_words

    def correct(self, text: str) -> GrammarResult:
        """
        Correct grammar in text. Never raises — returns result object.

        Returns original text on any failure (fail-open).
        """
        if not self.enabled:
            return GrammarResult(success=True, corrected=text, original=text)

        # Fast-path: skip short text (preserves trigger words like "easy", "simple")
        if not text or not text.strip() or len(text.split()) < self.min_words:
            return GrammarResult(success=True, corrected=text, original=text)

        t0 = time.monotonic()
        try:
            import ollama

            client = ollama.Client(host=self.host, timeout=self.timeout)

            response = client.generate(
                model=self.model,
                prompt=GRAMMAR_PROMPT.format(text=text),
                options={"num_predict": 256, "temperature": 0.1},
                think=False,  # Disable Qwen3 chain-of-thought
            )

            corrected = response.get("response", "").strip()
            duration = time.monotonic() - t0

            # Guard against hallucination: reject if length ratio is unreasonable
            if not corrected:
                return GrammarResult(
                    success=False, corrected=text, original=text,
                    duration_s=duration, error="Empty response from model",
                )

            ratio = len(corrected) / len(text)
            if ratio < 0.5 or ratio > 1.5:
                print(f"Grammar: rejected (length ratio {ratio:.2f})")
                return GrammarResult(
                    success=False, corrected=text, original=text,
                    duration_s=duration,
                    error=f"Length ratio {ratio:.2f} outside 0.5-1.5 range",
                )

            return GrammarResult(
                success=True, corrected=corrected, original=text,
                duration_s=duration,
            )

        except Exception as e:
            duration = time.monotonic() - t0
            print(f"Grammar error: {e}")
            return GrammarResult(
                success=False, corrected=text, original=text,
                duration_s=duration, error=str(e),
            )
