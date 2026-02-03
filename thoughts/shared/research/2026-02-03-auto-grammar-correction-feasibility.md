---
date: 2026-02-03T10:12:54-08:00
researcher: Claude Opus 4.5
git_commit: 5f4033f849cc53630b5c92024bf046ee80ea2d9c
branch: master
repository: dictate_agent
topic: "Auto Grammar Correction Feature: Architecture, Model Selection, and Rust vs Python Feasibility"
tags: [research, codebase, grammar-correction, ollama, whisper, transcription, rust, candle, pipeline]
status: complete
last_updated: 2026-02-03
last_updated_by: Claude Opus 4.5
---

# Research: Auto Grammar Correction for Dictate Agent

**Date**: 2026-02-03T10:12:54-08:00
**Researcher**: Claude Opus 4.5
**Git Commit**: 5f4033f849cc53630b5c92024bf046ee80ea2d9c
**Branch**: master
**Repository**: dictate_agent

## Research Question

From the perspective of a team of experts designing a dictation agent for someone with ADHD and high IQ: How should an auto grammar correction feature be implemented using a local, fast LLM (Ollama or equivalent) to correct grammar after or during the transcription workflow? Should it be implemented in Rust or Python?

## Summary

The dictate agent has a clear insertion point for grammar correction: between Whisper transcription output and the routing/output stage. The existing codebase already has a `_apply_corrections()` method in `transcribe.py:177-210` that does static string replacement — grammar correction would be a natural extension of this pattern but using neural inference instead of hardcoded replacements.

**Three viable implementation approaches exist**, ranked by latency:

1. **GECToR-style sequence tagging** (20-50ms) — Non-autoregressive, tags tokens with edit operations. Fastest possible approach.
2. **Qwen3-0.6b via Ollama** (50-150ms) — Already available in the pipeline. Reuses existing `LocalExecutor` infrastructure. Simplest integration.
3. **T5-small/base grammar model via ONNX** (100-200ms) — Proven grammar-specific fine-tuning available on HuggingFace.

**Rust feasibility**: A fully archived Rust implementation exists at `src_rust_archive/` with 9 complete source files and working Cargo.toml. The Candle ML framework (already used for Whisper in the archive) supports Qwen models and CUDA. A Rust grammar correction module is technically feasible but would require either (a) resurrecting the entire Rust codebase, or (b) building a standalone Rust binary/service that the Python daemon calls. The Python integration path is significantly simpler given the current Python codebase.

## Detailed Findings

### 1. Current Transcription Pipeline — Where Grammar Correction Fits

The pipeline flow in `main.py:135-168`:

```
Audio stop → transcribe → _apply_corrections() → route → handle_route
                              ↑
                    Grammar correction goes HERE
                    (transcribe.py:163 or main.py:160)
```

**Current correction mechanism** (`transcribe.py:177-210`):
- Static find/replace pairs for Whisper misrecognitions
- Handles "Clod"→"Claude", "Cloud"→"Claude", slash command normalization
- Called at `transcribe.py:163` after raw transcription, before returning `TranscriptionResult`

**Two insertion strategies**:

| Strategy | Location | Pros | Cons |
|----------|----------|------|------|
| Inside `Transcriber._apply_corrections()` | `transcribe.py:163` | Single responsibility, corrections grouped | Adds latency to transcription module, couples grammar to Whisper |
| In `DictateAgent._stop_recording()` | `main.py:160` (after `result.text` assignment) | Clean separation, can be toggled independently | Adds a new step in main orchestration |

### 2. Existing Ollama Infrastructure

The agent already has complete Ollama integration:

- **`local_executor.py`**: `LocalExecutor` class with `execute(prompt)` method using `ollama.Client`
- **`router.py`**: Uses Ollama `qwen3:0.6b` for request classification
- **`main.py:59-66`**: `ensure_ollama_running()` called at startup, `LocalExecutor` initialized with host/model/timeout
- **`config.py:34-56`**: `RouterConfig` has `ollama_host`, `ollama_model`, `ollama_timeout_s`

The grammar correction feature could reuse this infrastructure directly — the Ollama client is already initialized, the server is already ensured running, and the model is already pulled.

### 3. The Archived Rust Implementation

A complete Rust implementation exists at `src_rust_archive/` with 9 source files:

| Module | Status | Crate Dependencies |
|--------|--------|--------------------|
| `main.rs` | Complete entry point | tokio, tracing |
| `agent.rs` | Complete daemon + signal handling | tokio (signals, mpsc) |
| `config.rs` | Complete TOML config | config, toml, serde |
| `router.rs` | Complete Ollama + keyword routing | ollama-rs, tokio |
| `transcribe.rs` | Complete Whisper via Candle | candle-core/nn/transformers, hf-hub |
| `capture.rs` | Complete CPAL audio + resampling | cpal |
| `executor.rs` | Complete Claude CLI + NDJSON | tokio, serde_json |
| `output.rs` | Complete Enigo keyboard | enigo (x11rb) |
| `notify.rs` | Complete desktop notifications | notify-rust |

**Rust Candle supports Qwen models** — the `candle-transformers` crate includes Qwen architecture support. A grammar correction module in Rust would use:
- `candle-core` + `candle-nn` for tensor operations
- `candle-transformers` for model loading (Qwen or T5)
- `hf-hub` for downloading models from HuggingFace
- Quantization via candle's GGUF support

**Why Rust was archived**: The completion report (`thoughts/shared/project/2026-01-15-dictate-agent-completion.md`) documents the pivot: cudarc 0.16.6 was incompatible with CUDA 13.1, forcing CPU-only inference. Python was chosen for faster iteration with the PyTorch ecosystem.

### 4. Model Options for Grammar Correction

#### Option A: Qwen3-0.6b (Already Available)

- **Already pulled** for routing in the current setup
- **RTX 5080 performance**: Qwen3-4B achieves 188.90 tokens/s with Q4_K_M quantization, 0.10s first token latency, 5.8GB VRAM. The 0.6B model would be significantly faster.
- **Prompt approach**: `"Fix the grammar in this text, output only the corrected text: {transcription}"`
- **Latency estimate**: 50-150ms for short sentences on RTX 5080
- **Quality concern**: Research shows small LLMs (<2B) "struggle with retaining meaning and hallucinations" for grammar tasks (arxiv:2601.03874)

#### Option B: GECToR (Sequence Tagging)

- **Architecture**: Non-autoregressive — predicts edit operations (keep/delete/replace) per token using a Transformer encoder (RoBERTa/BERT)
- **Speed**: 10x faster than seq2seq approaches. Sub-50ms on GPU.
- **Accuracy**: F0.5 scores of 61-64 (CoNLL-2014), 68-72 (BEA-2019)
- **Implementation**: Available on GitHub (grammarly/gector), Python with PyTorch
- **Rust option**: Could run via ONNX Runtime (candle supports ONNX, or use `ort` crate)

#### Option C: T5-base Grammar Model

- **Model**: `vennify/t5-base-grammar-correction` on HuggingFace (126K downloads/month)
- **Size**: ~220M parameters (T5-base)
- **Input format**: Prefix with `"grammar: "` before text
- **Can be ONNX-exported** for faster inference (Microsoft demonstrated 2-3x speedup)

#### Option D: Hybrid Rule-Based + Neural

- **First pass**: Static corrections (already exists as `_apply_corrections()`)
- **Cache check**: Common phrases cached for <5ms response
- **Neural fallback**: Small model for novel inputs
- **Expected average**: 20-80ms with ~70% cache hit rate

### 5. Rust vs Python Decision Matrix

| Factor | Python | Rust |
|--------|--------|------|
| **Current codebase** | Active, all features work | Archived, not maintained |
| **Integration effort** | Add module, import in main.py | Either resurrect full codebase or build sidecar |
| **Ollama access** | Already initialized, client ready | Need ollama-rs or HTTP calls |
| **Model inference** | PyTorch/HuggingFace native | Candle (works but less ecosystem) |
| **ONNX Runtime** | `onnxruntime` pip package | `ort` crate |
| **Latency** | ~50-150ms (PyTorch GPU) | ~30-100ms (Candle GPU, theoretical) |
| **Maintainability** | Single codebase, consistent patterns | Two languages, two build systems |
| **ADHD-friendliness** | Faster to iterate, fewer yak-shaves | More upfront complexity |

### 6. ADHD-Specific Design Considerations

For a user with ADHD and high IQ:

1. **Zero-friction activation**: Grammar correction should be automatic, not requiring a trigger word. The user's working memory shouldn't be taxed with remembering to say "fix grammar."

2. **Invisible when working**: Corrections should happen silently between transcription and output. No notification unless correction was significant.

3. **Speed over perfection**: A fast, imperfect correction (Qwen3-0.6b, 50ms) beats a perfect but slow one (GPT-4, 2000ms). ADHD users are especially sensitive to latency — waiting kills flow state.

4. **Toggleable via config**: `[grammar] enabled = true` in config.toml. No route type needed, no trigger word — just a pipeline step.

5. **Bypass for code/commands**: Grammar correction should NOT apply to routes like HAIKU/SONNET/OPUS/COMMAND/TIMER — only to TYPE route (raw dictation) and potentially EDIT route. Technical terms should not be "corrected."

### 7. Hardware Capabilities

**Your system (Ryzen 9 9950X3D + RTX 5080 16GB)**:
- RTX 5080 benchmarks show Qwen3-4B at 188.90 tok/s, 0.10s TTFT
- Qwen3-0.6b would be ~3-5x faster (under 50ms for typical sentences)
- 16GB VRAM allows running grammar model alongside Whisper (which uses ~2-4GB)
- 3D V-Cache on CPU provides fast CPU-side model loading if needed

## Code References

- `dictate/transcribe.py:163` — Where `_apply_corrections()` is called (insertion point A)
- `dictate/transcribe.py:177-210` — Current static correction mechanism
- `dictate/main.py:150-161` — Stop recording flow, text available at line 160 (insertion point B)
- `dictate/main.py:173-233` — Route dispatch, TYPE handler at line 175-178
- `dictate/local_executor.py:22-87` — `LocalExecutor` pattern (reusable for grammar)
- `dictate/config.py:20-30` — `WhisperConfig` pattern (model for grammar config)
- `dictate/config.py:34-56` — `RouterConfig` with Ollama settings (reusable)
- `src_rust_archive/src/agent.rs:140` — Rust insertion point (between transcription and routing)
- `src_rust_archive/Cargo.toml:16-20` — Candle dependencies for Rust inference
- `.claude/skills/dictate-agent-developer/references/adding-executors.md` — Pattern guide for new executors
- `.claude/skills/dictate-agent-developer/references/architecture.md` — Module dependency rules

## Architecture Documentation

### Current Pipeline (Python)

```
$mod+n → SIGUSR1 → DictateAgent.toggle()
  → AudioCapture.start() / .stop()
  → Transcriber.transcribe(audio_path)
    → HuggingFace pipeline (Whisper large-v3-turbo, CUDA, chunked)
    → _apply_corrections() [static string replacements]
    → TranscriptionResult(text, language, duration_s)
  → Router.route(text)
    → First-word keyword matching → RouteType enum
  → _handle_route(route_result)
    → TYPE: OutputHandler.type_text()
    → HAIKU/SONNET/OPUS: ClaudeExecutor.execute() → type_text()
    → LOCAL: LocalExecutor.execute() → type_text()
    → TIMER: TimerExecutor.execute() → notification
```

### Proposed Pipeline (with Grammar Correction)

```
  → Transcriber.transcribe(audio_path)
    → _apply_corrections() [static]
    → TranscriptionResult
  → GrammarCorrector.correct(text)        ← NEW STEP
    → Ollama qwen3:0.6b (or dedicated model)
    → Returns corrected text
  → Router.route(corrected_text)
  → _handle_route(route_result)
```

### Module Pattern (from architecture docs)

New modules follow hub-and-spoke: `main.py` imports them, they don't import siblings. Config values passed through constructor, not imported from `config.py`. Result dataclass with `success`/`response`/`error` fields. Never raise exceptions — catch everything.

## Historical Context (from thoughts/)

- `thoughts/shared/project/2026-01-15-dictate-agent.md` — Original Rust specification. Detailed EDIT mode spec includes "fix: correct the grammar" as an example voice command. Grammar correction was conceptualized as an explicit user command, not automatic.
- `thoughts/shared/project/2026-01-15-dictate-agent-completion.md` — Documents Python pivot. Rust abandoned due to cudarc/CUDA incompatibility.
- `thoughts/shared/research/2026-01-30-whisper-long-form-transcription.md` — Speculative decoding incompatible with chunked pipeline. Relevant because grammar correction adds latency on top of an already-optimized pipeline.
- `thoughts/shared/research/2026-02-02-dictate-agent-skill-feasibility.md` — Documents 3-file pattern for adding route types and executor pattern. Grammar correction doesn't need a new route type — it's a pipeline step, not a route.
- `thoughts/shared/handoffs/general/2026-01-21_13-35-03_audio-recording-cutoff-debug.md` — Known 50-100ms audio cutoff bug. Grammar correction latency stacks on top of this.

## Related Research

- `thoughts/shared/research/2026-01-15-phase-1-research.md` — Original Rust library research (candle, cpal, enigo)
- `thoughts/shared/research/2026-01-23-interruption-keybind.md` — SIGUSR2 cancel (implemented)

## Expert Team Assessment

### Recommended Approach: Python + Qwen3-0.6b via Ollama

**Why this wins for an ADHD user with high IQ:**

1. **Immediate gratification**: Reuses existing Ollama infrastructure. No new dependencies, no new build systems, no yak-shaving.
2. **Fast enough**: 50-150ms on RTX 5080. Below the 300ms threshold where latency becomes perceptible in voice UI.
3. **Configurable**: A `[grammar]` section in config.toml with `enabled = true`, `model = "qwen3:0.6b"`, `only_type_route = true`.
4. **Escape hatch to Rust**: If latency matters, a standalone Rust binary using Candle + Qwen can be called as a subprocess (same pattern as `claude` CLI in `executor.py`). This avoids resurrecting the entire Rust codebase.
5. **Upgradeable**: Start with Qwen3-0.6b, swap to a fine-tuned GECToR or T5 model later via config change.

### If Rust Is Strongly Preferred

Build a standalone `grammar-fix` CLI binary:
- Uses `candle-core`, `candle-nn`, `candle-transformers` (same deps as archived transcribe.rs)
- Takes text on stdin, outputs corrected text on stdout
- Python calls it via `subprocess.run(["grammar-fix"], input=text, capture_output=True)`
- Gets Rust performance without resurrecting the full daemon
- Can be gradually extended to absorb more of the pipeline

### Implementation Complexity Estimate

| Approach | New Files | Modified Files | New Dependencies |
|----------|-----------|----------------|------------------|
| Python + Ollama | 1 (`grammar.py`) | 2 (`main.py`, `config.py`) | None |
| Python + GECToR | 1 (`grammar.py`) | 2 (`main.py`, `config.py`) | `gector`, `allennlp` |
| Python + T5/ONNX | 1 (`grammar.py`) | 2 (`main.py`, `config.py`) | `onnxruntime` |
| Rust sidecar | 1 Rust project + 1 (`grammar.py`) | 2 (`main.py`, `config.py`) | Candle crates |
| Full Rust resurrection | 10+ Rust files | N/A (new codebase) | All Cargo.toml deps |

## Open Questions

1. **Should grammar correction apply to all routes or only TYPE?** Correcting text before sending to Claude may alter intent. Code snippets and technical terms could be mangled.
2. **Should the original (uncorrected) text be logged for debugging?** Useful for tuning the model but adds log noise.
3. **Is Qwen3-0.6b grammar quality acceptable?** Research suggests small LLMs struggle with complex corrections. May need empirical testing with dictation samples.
4. **Should a dedicated grammar model (GECToR/T5) be preferred over a general LLM?** GECToR is 10x faster but can't handle voice-specific artifacts (repeated words, filler words, missing punctuation).
5. **Cache strategy?** Common corrections could be cached to avoid redundant inference. How large should the cache be?
