"""
Grammar correction via Ollama for post-transcription cleanup.

Sits between transcription and routing in the pipeline.
Fail-open: if Ollama is unavailable, returns original text unchanged.
"""

from dataclasses import dataclass
from typing import Optional


@dataclass
class GrammarResult:
    """Result from grammar correction."""

    success: bool
    corrected: str
    original: str
    error: Optional[str] = None


GRAMMAR_PROMPT = (
    "Fix grammar and punctuation in the following text. "
    "Output ONLY the corrected text with no explanation. "
    "If the text is already correct, output it unchanged.\n\n"
    "{text}"
)


class GrammarCorrector:
    """Corrects grammar on transcribed text via Ollama."""

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
        Correct grammar in text via Ollama.

        Returns GrammarResult with corrected text. On any failure,
        returns original text unchanged (fail-open).
        """
        if not text or not text.strip():
            return GrammarResult(success=True, corrected=text, original=text)

        if len(text.split()) < self.min_words:
            return GrammarResult(success=True, corrected=text, original=text)

        try:
            import ollama

            client = ollama.Client(host=self.host, timeout=self.timeout)

            response = client.generate(
                model=self.model,
                prompt=GRAMMAR_PROMPT.format(text=text),
                options={
                    "num_predict": 256,
                    "temperature": 0.1,
                },
                think=False,
            )

            corrected = response.get("response", "").strip()

            if not corrected:
                return GrammarResult(
                    success=False, corrected=text, original=text,
                    error="Empty response from model",
                )

            # Guard: if response length is wildly different, model hallucinated
            orig_len = len(text)
            if orig_len > 0 and (len(corrected) > orig_len * 1.5 or len(corrected) < orig_len * 0.5):
                print(f"Grammar: response length ratio suspicious, using original")
                return GrammarResult(
                    success=False, corrected=text, original=text,
                    error="Response length diverged too much from original",
                )

            return GrammarResult(success=True, corrected=corrected, original=text)

        except ImportError:
            return GrammarResult(
                success=False, corrected=text, original=text,
                error="ollama package not installed",
            )
        except Exception as e:
            error_msg = str(e)
            if "connection" in error_msg.lower() or "refused" in error_msg.lower():
                error_msg = f"Cannot connect to Ollama at {self.host}"
            print(f"Grammar correction failed: {error_msg}")
            return GrammarResult(
                success=False, corrected=text, original=text,
                error=error_msg,
            )
