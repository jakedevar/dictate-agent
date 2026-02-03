# Adding a Pipeline Step

Pipeline steps are **fail-open middleware** that transform text between transcription and routing. They differ from executors (which handle routes) and route types (which classify text). Pipeline steps are transparent to routing — they modify the text before it reaches the router.

## When to Use This Pattern

Use a pipeline step when you need to transform **all** transcribed text before it's routed, regardless of what route it ends up on. Examples:
- Grammar correction (implemented: `grammar.py`)
- Profanity filtering
- Language detection/translation
- Text normalization

If your transformation should only apply to a specific route, put it in that route's executor instead.

## Result Dataclass

Pipeline steps use a slightly different result pattern than executors — they carry both the `original` and `corrected` text:

```python
from dataclasses import dataclass
from typing import Optional

@dataclass
class StepResult:
    """Result from pipeline step."""
    success: bool
    corrected: str        # Always safe to use (original on failure)
    original: str
    error: Optional[str] = None
```

**Key invariant**: `corrected` is always safe to use. On failure, it equals `original`.

## Step Class

```python
class NewStep:
    """Description of what this step does."""

    def __init__(self, enabled: bool = True, ...):
        self.enabled = enabled
        # Accept all config through constructor (no global access)

    def correct(self, text: str) -> StepResult:
        """Transform text. Never raises — returns result object."""
        # Fast-path: skip empty/short text
        if not text or not text.strip():
            return StepResult(success=True, corrected=text, original=text)

        try:
            # Do transformation...
            return StepResult(success=True, corrected=result, original=text)

        except Exception as e:
            # FAIL-OPEN: return original text on any error
            return StepResult(
                success=False, corrected=text, original=text,
                error=str(e),
            )
```

### Key conventions:
- **Fail-open**: On ANY error, return original text. Never block the pipeline.
- **Fast-path short text**: Skip transformation for very short inputs (preserves trigger words like "easy", "simple")
- **Guard against hallucination**: If using an LLM, check that the output length is within a reasonable ratio of the input
- **Never raise exceptions** — same as executors
- **Print status** for daemon log visibility

## Ollama-Specific Notes

If your step uses Ollama (like grammar correction):

```python
response = client.generate(
    model=self.model,
    prompt=PROMPT.format(text=text),
    options={"num_predict": 256, "temperature": 0.1},
    think=False,  # Disable Qwen3 chain-of-thought for raw output
)
```

- Use `think=False` — Qwen3 models default to chain-of-thought mode which consumes tokens and may produce empty responses in `generate()` mode
- The `/no_think` prompt suffix only works with chat completions, NOT `generate()`
- Low temperature (0.1) constrains the model to near-deterministic corrections
- `num_predict: 256` caps output length as a safety measure

## Integration in main.py

### 1. Import at top

```python
from .grammar import GrammarCorrector
```

### 2. Initialize after `ensure_ollama_running()` (if using Ollama)

```python
self.grammar = GrammarCorrector(
    host=self.config.router.ollama_host,  # Reuse router's host
    model=self.config.grammar.model,
    timeout_s=self.config.grammar.timeout_s,
    enabled=self.config.grammar.enabled,
    min_words=self.config.grammar.min_words,
)
```

### 3. Call in `_stop_recording()` between transcription and routing

Pipeline steps go in this exact location (main.py:172-179):

```python
text = result.text
print(f"Transcribed: {text}")

# Grammar correction (fail-open: original text on failure)
if self.grammar.enabled:
    grammar_result = self.grammar.correct(text)
    if grammar_result.success and grammar_result.corrected != grammar_result.original:
        print(f"Grammar corrected: {grammar_result.corrected}")
    elif grammar_result.error:
        print(f"Grammar skipped: {grammar_result.error}")
    text = grammar_result.corrected

# Route the text
route_result = self.router.route(text)
```

**Ordering**: If multiple pipeline steps exist, they chain sequentially. Each step receives the output of the previous step. Order matters — grammar correction should run before any content filtering.

## Configuration

Follow [config-patterns.md](config-patterns.md). Pipeline steps get their own `[section]` in config TOML. The `enabled` field is mandatory — users must be able to disable any pipeline step.

## Existing Pipeline Steps

| Step | Module | Uses | Config Section |
|------|--------|------|----------------|
| Grammar correction | `grammar.py` | Ollama (qwen3:0.6b) | `[grammar]` |
