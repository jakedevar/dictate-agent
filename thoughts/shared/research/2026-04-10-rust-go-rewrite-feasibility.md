---
date: 2026-04-10T12:00:00-05:00
researcher: claude
git_commit: 42f83e88a61bc94b2422d56e8cc658dcfdc8505a
branch: master
repository: dictate_agent
topic: "Rewriting dictate-agent in Rust"
tags: [research, codebase, rewrite, rust, feasibility, whisper, systems-programming]
status: complete
last_updated: 2026-04-10
last_updated_by: claude
last_updated_note: "Finalized Rust decision, removed Go analysis, added Whisper performance benchmarks from real data"
---

# Research: Rewriting dictate-agent in Rust

**Date**: 2026-04-10
**Researcher**: claude
**Git Commit**: 42f83e88a61bc94b2422d56e8cc658dcfdc8505a
**Branch**: master
**Repository**: dictate_agent

## Research Question

Can the dictate-agent Python project be rewritten in Rust? What are the ecosystem options, and what is the performance impact of moving Whisper inference to whisper.cpp?

## Decision

**Rewrite in Rust using whisper-rs (Option A: direct whisper.cpp, no sidecar).**

The dictate-agent codebase is 2,412 LOC across 13 Python modules. It is fundamentally a **signal-driven daemon** that orchestrates external tools (parec, xdotool, xclip, playerctl, systemd-run, claude CLI) via subprocess calls, with two heavier integrations: Whisper transcription (HuggingFace transformers + PyTorch) and Ollama HTTP API.

Rust is the right choice because:

1. **Excellent ecosystem coverage** — mature, high-download crates exist for every requirement: audio (`cpal` 926k/mo), X11 input (`enigo` 104k/mo), notifications (`notify-rust` 582k/mo), signals (`signal-hook` 11.8M/mo), config (`toml` 40.7M/mo)
2. **Single static binary** — no runtime, aligns with the original project spec. The systemd service file already points to `target/release/dictate-agent`
3. **Eliminates subprocess calls** — Rust crates replace parec (cpal), xdotool (enigo), and notify-send (notify-rust) with native library calls
4. **Whisper via whisper-rs is fast enough** — real-world benchmarking shows the latency increase is ~200-400ms, well under perceptible threshold for this use case

## Current Codebase Analysis

### Architecture Overview

The project is a synchronous, signal-driven daemon. The main loop calls `signal.pause()` in a tight `while self.running` loop. All processing happens inside signal handlers:

```
SIGUSR1 → toggle recording (start/stop)
SIGUSR2 → cancel recording (discard)
SIGINT/SIGTERM → graceful shutdown
```

### Module Inventory (13 modules, 2,412 total LOC)

| Module | LOC | Purpose | Key Dependencies |
|--------|-----|---------|-----------------|
| `main.py` | 471 | Daemon orchestration, signal handling | All internal modules |
| `timer_executor.py` | 274 | Duration parsing, systemd-run timers | `subprocess`, `re` |
| `transcribe.py` | 244 | Whisper STT via HuggingFace | `torch`, `transformers` |
| `config.py` | 232 | TOML config dataclasses | `tomllib`/`tomli` |
| `status_window.py` | 222 | Tkinter floating overlay | `tkinter`, `threading` |
| `history.py` | 205 | SQLite interaction logging | `sqlite3` |
| `local_executor.py` | 182 | Ollama local inference | `ollama` (HTTP client) |
| `notify.py` | 111 | Notification output (stdout) | None |
| `grammar.py` | 106 | Ollama grammar correction | `ollama` |
| `audio.py` | 94 | Audio recording via parecord | `subprocess` |
| `output.py` | 92 | Text typing via xclip + xdotool | `subprocess` |
| `router.py` | 80 | Keyword-based request routing | None |
| `__init__.py` | 3 | Package marker | None |

### External Program Dependencies

| Program | Called From | How | Purpose |
|---------|-----------|-----|---------|
| `parecord` | `audio.py` | `subprocess.Popen` | Audio recording |
| `playerctl` | `main.py` | `subprocess.run` | Media pause/resume |
| `xclip` | `output.py` | `subprocess.run` | Clipboard read/write |
| `xdotool` | `output.py` | `subprocess.run` | Paste keystroke (ctrl+v) |
| `systemd-run` | `timer_executor.py` | `subprocess.run` | Transient timers |
| `dunstify` | `timer_executor.py` | via bash string | Persistent notifications |
| `play` (sox) | `timer_executor.py` | via bash string | Timer alarm sound |
| `ollama` | `local_executor.py` | `subprocess.Popen` | Auto-start Ollama server |
| `claude` | (via executor) | subprocess | Claude Code CLI |

### Python Package Dependencies

| Package | Used For | Heavyweight? |
|---------|---------|-------------|
| `torch` | GPU detection, dtype | **Yes** - ~2GB |
| `transformers` | Whisper model + pipeline | **Yes** - complex |
| `accelerate` | Model distribution | Moderate |
| `optimum` | Model optimization | Moderate |
| `ollama` | HTTP client for Ollama | Lightweight |
| `tomli` | TOML parsing (fallback) | Lightweight |
| `tkinter` | Status window overlay | Stdlib |
| `sqlite3` | History database | Stdlib |

**Key insight**: The only truly heavyweight Python dependencies are `torch` and `transformers` for Whisper inference. Everything else is lightweight or stdlib. The Rust rewrite eliminates the entire PyTorch/transformers stack.

## Whisper Performance Analysis

### Current Python Implementation

Running **whisper-large-v3-turbo** on an **RTX 5080 (16GB)** via HuggingFace transformers with SDPA attention, FP16. Speculative decoding is **configured but disabled** — the code at `transcribe.py:103` explicitly skips it:

```python
if self.config.use_speculative_decoding:
    print("Speculative decoding skipped (incompatible with chunked pipeline)")
```

**Real-world performance from 3,595 recorded interactions:**

| Metric | Value |
|--------|-------|
| **Average transcription time** | **0.183 seconds** |
| **Fastest** | 0.066 seconds |
| **Slowest** | 2.92 seconds |
| **Average total pipeline** | 0.87 seconds |

Most transcriptions complete in 100-200ms. The speculative decoding "loss" discussed in early research is moot — it was never active.

### Expected Performance with whisper-rs (whisper.cpp)

whisper.cpp's CUDA path is less optimized than HuggingFace transformers for large models. Community benchmarks show whisper.cpp GPU being ~2-3x slower than faster-whisper/CTranslate2 for the same model. However, the RTX 5080 is fast enough that absolute numbers remain well under 1 second.

**Projected latency on RTX 5080:**

| Audio Length | Current (HF transformers) | whisper-rs (estimated) | Delta |
|-------------|--------------------------|----------------------|-------|
| 5 seconds | ~0.10s | ~0.20-0.30s | +0.1-0.2s |
| 10 seconds | ~0.15s | ~0.30-0.50s | +0.15-0.35s |
| 20 seconds | ~0.25s | ~0.50-0.80s | +0.25-0.55s |
| 30 seconds | ~0.40s | ~0.80-1.20s | +0.4-0.8s |

**Estimated average with whisper-rs: ~400-600ms** (vs current ~183ms).

### Why This Trade-Off Is Acceptable

1. **Still sub-second** — 400-600ms transcription is imperceptible when followed by Ollama routing (~200ms) and Claude response streaming (seconds)
2. **Lower memory** — whisper.cpp GGUF models use ~2.5GB VRAM vs PyTorch FP16 at ~5GB
3. **Faster cold start** — whisper.cpp loads models in ~3-5s vs PyTorch's ~15-30s
4. **Single binary** — no Python, no venv, no torch installation, no dependency hell
5. **Transcription is not the bottleneck** — in the 0.87s average pipeline, transcription (0.18s) is only 21% of total time. Even at 0.5s it would be 40% — the rest is Ollama + Claude

### Benchmark Sources

- faster-whisper large-v3-turbo on RTX 2080 Ti: ~40x real-time (19.2s for 13 minutes of audio) — [SYSTRAN/faster-whisper #1030](https://github.com/SYSTRAN/faster-whisper/issues/1030)
- whisper.cpp CUDA is ~2-3x slower than CTranslate2 for large models — community consensus across multiple GitHub issues
- HuggingFace speculative decoding: 2.2x speedup on T4 GPU — [HuggingFace Blog](https://huggingface.co/blog/whisper-speculative-decoding) (not applicable since it was never active in our codebase)
- HTTP localhost overhead: <1ms with connection reuse — negligible for any sidecar consideration
- Model loading: whisper.cpp GGUF ~3-5s, HuggingFace transformers ~8-30s depending on hardware

## Rust Crate Selection

### Confirmed Dependencies

```toml
[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# Audio
cpal = { version = "0.17", features = ["pipewire"] }

# Whisper
whisper-rs = "0.16"

# Ollama
ollama-rs = "0.3"

# X11 / Input
enigo = "0.6"

# Notifications
notify-rust = "4.14"

# Signals
signal-hook = "0.4"
signal-hook-tokio = "0.3"

# Config
toml = "1.1"
serde = { version = "1", features = ["derive"] }

# SQLite
rusqlite = { version = "0.31", features = ["bundled"] }

# Subprocess NDJSON
serde_json = "1"

# Systemd (optional, for native timer creation)
zbus_systemd = { version = "0.26", optional = true }
```

### Crate Details

| Requirement | Crate | Version | Downloads/mo | Maturity | Notes |
|-------------|-------|---------|-------------|----------|-------|
| Audio capture | `cpal` | 0.17.3 | 926k | Production | PipeWire feature flag eliminates parec subprocess |
| Audio capture (alt) | `pipewire` | 0.9.2 | 68k | Functional | Direct PipeWire bindings, some rough edges |
| Whisper STT | `whisper-rs` | 0.16.0 | 63k | Good | Binds whisper.cpp via build.rs, CUDA support |
| Ollama client | `ollama-rs` | 0.3.4 | 27k | Good | Full API: generate, chat, streaming, embeddings |
| Subprocess/async | `tokio::process` | (tokio 1.x) | 200M+ | Production | Async subprocess with piped stdout |
| NDJSON parsing | `serde_json` | 1.x | 300M+ | Production | Line-by-line decode on async stdout |
| Keyboard input | `enigo` | 0.6.1 | 104k | Good | Cross-platform, X11 stable, Wayland experimental |
| X11 bindings | `x11rb` | 0.13.2 | 3.1M | Production | Low-level alternative to enigo |
| Notifications | `notify-rust` | 4.14.0 | 582k | Production | Pure Rust D-Bus, XDG desktops |
| Signal handling | `signal-hook` | 0.4.4 | 11.8M | Production | SIGUSR1/2 native, tokio bridge available |
| TOML config | `toml` | 1.1.2 | 40.7M | Production | Used by Cargo itself, serde derive |
| SQLite | `rusqlite` | 0.31 | millions | Production | Bundled SQLite, WAL mode support |
| MPRIS media | `mpris` | 2.0.1 | 6k | Stale | Shell out to playerctl instead |
| Systemd | `zbus_systemd` | 0.26.0 | 136k | Good | Native D-Bus to systemd |

## Module Mapping (Python to Rust)

| Python Module | Rust Module | Key Changes |
|---------------|-------------|-------------|
| `main.py` | `src/main.rs` + `src/agent.rs` | Tokio async main, signal stream select |
| `audio.py` | `src/audio.rs` | cpal PCM capture (no subprocess) |
| `transcribe.py` | `src/transcribe.rs` | whisper-rs (no threading Event, sync FFI) |
| `config.py` | `src/config.rs` | serde + toml derive macros |
| `router.py` | `src/router.rs` | Direct port, simple match arms |
| `output.py` | `src/output.rs` | enigo for typing (no subprocess) |
| `notify.py` | `src/notify.rs` | notify-rust (no subprocess) |
| `local_executor.py` | `src/ollama.rs` | ollama-rs async client |
| `grammar.py` | `src/grammar.rs` | ollama-rs with temperature config |
| `timer_executor.py` | `src/timer.rs` | Duration parser + subprocess systemd-run |
| `history.py` | `src/history.rs` | rusqlite with WAL mode |
| `status_window.py` | `src/status.rs` | TBD: egui overlay or separate process |

## Migration Strategy

1. **Phase 1**: Port config, router, notify, output, history (simplest modules, ~600 LOC)
2. **Phase 2**: Port main daemon loop with signal handling (core architecture)
3. **Phase 3**: Port audio capture with cpal (replace subprocess)
4. **Phase 4**: Port Whisper transcription with whisper-rs
5. **Phase 5**: Port Ollama integration (local executor + grammar)
6. **Phase 6**: Port timer executor (duration parser + systemd-run)
7. **Phase 7**: Replace or redesign status window

Estimated effort: 3,000-4,000 LOC of Rust (Rust is typically 1.5-2x the LOC of Python for equivalent functionality due to explicit error handling and type annotations).

## Risks and Concerns

1. **Whisper latency increase** — ~400-600ms vs ~183ms average. Acceptable given full pipeline is 0.87s and transcription is not the bottleneck. Monitor after migration.
2. **Status window** — Tkinter replacement in Rust is non-trivial. Consider a separate lightweight process (simple X11 window with `x11rb`) or `egui` with X11 override-redirect.
3. **Whisper model loading time** — The Python version loads models in a background thread. Rust will need similar async initialization via Tokio spawn_blocking.
4. **whisper-rs maintenance** — The crate migrated from GitHub to Codeberg. Monitor for ecosystem fragmentation.
5. **Build complexity** — whisper-rs requires CUDA toolkit for GPU acceleration. CI/CD and developer setup need to account for this.
6. **GGUF model format** — whisper-rs uses GGUF models, not the HuggingFace safetensors format. Need to download or convert the whisper-large-v3-turbo model to GGUF.

## Open Questions

1. Should the status window be a separate process (simpler) or embedded in the main binary (single binary goal)?
2. Should we support Wayland in the rewrite (future-proofing), or keep X11-only?
3. What's the target for model loading time? whisper.cpp should be faster (~3-5s vs ~15-30s).
4. Should the rewrite maintain backward compatibility with the existing config.toml format?
5. Should we use `distil-large-v3` in GGUF format for even faster inference, or stick with `large-v3-turbo`?

## Related Research

- `thoughts/shared/research/2026-01-15-phase-1-research.md` — Original Phase 1 research
- `thoughts/shared/research/2026-01-30-whisper-long-form-transcription.md` — Whisper transcription analysis
- `thoughts/shared/project/2026-01-15-dictate-agent.md` — Original project spec (Rust-first design)
- `thoughts/shared/project/2026-01-15-dictate-agent-completion.md` — Completion report (Python MVP)

## Code References

- `dictate/main.py` — Central daemon orchestrator (471 LOC, signal handlers at lines 460-463)
- `dictate/audio.py` — Audio capture via parecord subprocess (94 LOC)
- `dictate/transcribe.py` — Whisper inference via HuggingFace (244 LOC, model loading at lines 73-131, speculative decoding skip at line 103)
- `dictate/config.py` — TOML config with 9 dataclasses (232 LOC)
- `dictate/output.py` — xclip + xdotool clipboard-paste approach (92 LOC)
- `dictate/timer_executor.py` — Duration parser + systemd-run (274 LOC, most complex module)
- `dictate/status_window.py` — Tkinter overlay with threading (222 LOC)
- `dictate/history.py` — SQLite WAL-mode history (205 LOC, timing data at transcription_duration_s column)
- `dictate/local_executor.py` — Ollama client wrapper (182 LOC)
- `dictate/grammar.py` — Ollama grammar correction (106 LOC)
- `dictate/router.py` — Keyword prefix routing (80 LOC)
- `systemd/dictate-agent.service` — Already points to `target/release/dictate-agent` (Rust binary path)
