---
date: 2026-04-10T12:00:00-05:00
researcher: claude
git_commit: 42f83e88a61bc94b2422d56e8cc658dcfdc8505a
branch: master
repository: dictate_agent
topic: "Feasibility of rewriting dictate-agent in Rust or Go"
tags: [research, codebase, rewrite, rust, go, feasibility, whisper, systems-programming]
status: complete
last_updated: 2026-04-10
last_updated_by: claude
---

# Research: Feasibility of Rewriting dictate-agent in Rust or Go

**Date**: 2026-04-10
**Researcher**: claude
**Git Commit**: 42f83e88a61bc94b2422d56e8cc658dcfdc8505a
**Branch**: master
**Repository**: dictate_agent

## Research Question

Can the dictate-agent Python project be rewritten in Rust or Go? Rust is preferred, but Go is acceptable if it's a better fit.

## Summary

**Recommendation: Rust** — with one significant caveat around Whisper speculative decoding.

The dictate-agent codebase is 2,412 LOC across 13 Python modules. It is fundamentally a **signal-driven daemon** that orchestrates external tools (parec, xdotool, xclip, playerctl, systemd-run, claude CLI) via subprocess calls, with two heavier integrations: Whisper transcription (HuggingFace transformers + PyTorch) and Ollama HTTP API. Both Rust and Go can handle the daemon/subprocess orchestration equally well. The deciding factors are:

1. **Rust has better ecosystem coverage** — more mature, higher-download crates for audio, X11, notifications, and signal handling
2. **Rust produces a single static binary** — no runtime, no CGo quirks, aligns with the original project spec
3. **Both languages share the same critical gap** — Whisper speculative decoding is not available outside the Python HuggingFace ecosystem
4. **Go's only advantage is Ollama** — the official Ollama client is Go-native, but `ollama-rs` for Rust is mature enough at 27k downloads/month

The rewrite is **feasible in either language** for everything except speculative decoding. The recommended architecture uses Rust for the daemon with whisper.cpp (via `whisper-rs`) accepting the latency trade-off, or an optional Python/whisper.cpp HTTP sidecar for speculative decoding if latency is critical.

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

**Key insight**: The only truly heavyweight Python dependencies are `torch` and `transformers` for Whisper inference. Everything else is lightweight or stdlib.

## Detailed Findings

### Requirement-by-Requirement Ecosystem Comparison

#### 1. Audio Capture

| | Rust | Go |
|---|---|---|
| **Best option** | `cpal` v0.17.3 (926k downloads/mo) | `parec` subprocess |
| **Alternative** | `pipewire` v0.9.2 (68k/mo) | `malgo` (miniaudio bindings, 398 stars) |
| **Native capture?** | Yes — cpal with `pipewire` feature flag | No mature PipeWire bindings |
| **Verdict** | **Rust wins** — eliminates parec subprocess entirely | Must shell out or use CGo |

#### 2. Whisper Speech-to-Text (THE CRITICAL REQUIREMENT)

| | Rust | Go |
|---|---|---|
| **Best option** | `whisper-rs` v0.16.0 (63k/mo) | whisper.cpp Go bindings (official, in-tree) |
| **Speculative decoding?** | **No** — whisper.cpp doesn't support it | **No** — same underlying engine |
| **Build complexity** | Link against whisper.cpp via build.rs | CGo, manual `make`, `CGO_CFLAGS` |
| **Quality** | Same model weights = same WER | Same |
| **Performance concern** | None reported | Historical 45x slowdown (may be fixed) |
| **Verdict** | **Rust wins** — cleaner FFI, no CGo, same limitation | More awkward build, same limitation |

**Speculative decoding gap**: Both languages lose the 2x speedup from HuggingFace's distil-whisper assistant model. whisper.cpp upstream has an open, unresponded feature request (#2436, Sep 2024). Mitigations:
- Accept ~2x slower transcription (still fast with GPU)
- Use distil-large-v3 directly (6x faster than large-v3, ~1% WER cost)
- Run whisper.cpp's built-in HTTP server as a sidecar
- Keep Python transcription as a sidecar and rewrite only orchestration

#### 3. Ollama LLM Client

| | Rust | Go |
|---|---|---|
| **Best option** | `ollama-rs` v0.3.4 (27k/mo, 960 stars) | `ollama/ollama/api` (official, Go-native) |
| **Streaming?** | Yes | Yes |
| **Verdict** | Good enough | **Go wins** — official client, same codebase as Ollama |

#### 4. Claude Code CLI + NDJSON Streaming

| | Rust | Go |
|---|---|---|
| **Approach** | `tokio::process::Command` + `serde_json` | `os/exec` + `encoding/json` |
| **Streaming** | Async stdout via tokio-stream | `json.NewDecoder` on stdout pipe |
| **Verdict** | **Tie** — both excellent, arguably cleaner than Python |

#### 5. X11 Keyboard Simulation

| | Rust | Go |
|---|---|---|
| **Best option** | `enigo` v0.6.1 (104k/mo) | `robotgo` (10.7k stars) or subprocess |
| **Alternative** | `x11rb` v0.13.2 (3.1M/mo) | `jezek/xgb` (xgb fork) |
| **Verdict** | **Rust wins** — enigo is higher-level, no CGo |

#### 6. Desktop Notifications

| | Rust | Go |
|---|---|---|
| **Best option** | `notify-rust` v4.14.0 (582k/mo) | subprocess or `godbus/dbus` |
| **Verdict** | **Rust wins** — pure Rust, D-Bus native, very mature |

#### 7. Signal Handling

| | Rust | Go |
|---|---|---|
| **Best option** | `signal-hook` v0.4.4 (11.8M/mo) | `os/signal` (stdlib) |
| **SIGUSR1/2?** | Yes, native | Yes, native |
| **Verdict** | **Tie** — both excellent |

#### 8. TOML Configuration

| | Rust | Go |
|---|---|---|
| **Best option** | `toml` v1.1.2 (40.7M/mo) | `pelletier/go-toml/v2` |
| **Serde integration?** | Yes — `#[derive(Deserialize)]` | Reflection-based |
| **Verdict** | **Rust wins** — toml crate is used by Cargo itself |

#### 9. Media Playback Control (MPRIS)

| | Rust | Go |
|---|---|---|
| **Best option** | `mpris` v2.0.1 (stale) or subprocess | `godbus/dbus` v5.2.2 or subprocess |
| **Verdict** | **Tie** — both should shell out to playerctl |

#### 10. Systemd Integration

| | Rust | Go |
|---|---|---|
| **Best option** | `zbus_systemd` v0.26.0 (136k/mo) | `coreos/go-systemd/v22` v22.7.0 (2.7k stars) |
| **Verdict** | **Tie** — both mature |

#### 11. Status Window (Tkinter replacement)

| | Rust | Go |
|---|---|---|
| **Approach** | `iced`, `egui`, or X11 override-redirect window | GTK bindings or X11 window |
| **Complexity** | Medium — simple overlay window | Medium |
| **Verdict** | **Tie** — both need work, consider separate process |

#### 12. SQLite History

| | Rust | Go |
|---|---|---|
| **Best option** | `rusqlite` (millions/mo) | `mattn/go-sqlite3` (CGo) or `modernc.org/sqlite` (pure Go) |
| **Verdict** | **Rust wins** — rusqlite is mature, no CGo option available |

### Consolidated Scorecard

| Category | Rust | Go | Winner |
|----------|------|-----|--------|
| Audio capture | cpal (native) | parec subprocess | Rust |
| Whisper STT | whisper-rs (clean FFI) | whisper.cpp CGo (awkward) | Rust |
| Ollama client | ollama-rs (good) | Official Go client | Go |
| Claude CLI/NDJSON | tokio (excellent) | os/exec (excellent) | Tie |
| X11 keyboard | enigo (excellent) | robotgo/subprocess | Rust |
| Notifications | notify-rust (excellent) | subprocess/godbus | Rust |
| Signal handling | signal-hook (excellent) | os/signal (excellent) | Tie |
| TOML config | toml (canonical) | go-toml (good) | Rust |
| Media control | subprocess | subprocess | Tie |
| Systemd | zbus_systemd (good) | go-systemd (excellent) | Tie |
| Status window | egui/iced | GTK/X11 | Tie |
| SQLite | rusqlite (excellent) | go-sqlite3 (CGo) | Rust |
| **Score** | **6 wins** | **1 win** | **Rust** |

## Recommended Architecture (Rust)

### Crate Selection

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

### Module Mapping (Python → Rust)

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

### Whisper Strategy Options

**Option A (Recommended): Direct whisper-rs, accept latency trade-off**
- Use `whisper-rs` with `distil-large-v3` model directly
- ~6x faster than large-v3, only ~1% WER degradation
- No speculative decoding needed because the distilled model is already fast
- Single binary, zero Python dependency

**Option B: Sidecar architecture**
- Rust daemon handles everything except transcription
- whisper.cpp server runs as a separate process (HTTP API on localhost)
- Daemon sends audio file via HTTP, receives transcription
- Preserves option for future speculative decoding

**Option C: Keep Python transcription as sidecar**
- Rust daemon + Python venv with just torch/transformers
- Maximum transcription quality with speculative decoding
- Defeats some of the single-binary goal

### Migration Strategy

1. **Phase 1**: Port config, router, notify, output, history (simplest modules, ~600 LOC)
2. **Phase 2**: Port main daemon loop with signal handling (core architecture)
3. **Phase 3**: Port audio capture with cpal (replace subprocess)
4. **Phase 4**: Port Whisper transcription with whisper-rs
5. **Phase 5**: Port Ollama integration (local executor + grammar)
6. **Phase 6**: Port timer executor (duration parser + systemd-run)
7. **Phase 7**: Replace or redesign status window

Estimated effort: 3,000-4,000 LOC of Rust (Rust is typically 1.5-2x the LOC of Python for equivalent functionality due to explicit error handling and type annotations).

## Why Not Go?

Go is viable but loses on several fronts:

1. **CGo friction** — whisper.cpp Go bindings require manual `make`, `CGO_CFLAGS`, and have reported performance issues. Rust FFI to whisper.cpp is cleaner via `whisper-rs` build.rs automation.
2. **Fewer native replacements** — Go still needs to shell out for audio (no mature PipeWire bindings), X11 keyboard input (robotgo requires CGo), and notifications. Rust can eliminate most subprocess calls.
3. **Original project spec was Rust** — The systemd service file already points to `target/release/dictate-agent`. The project was designed for Rust from the start.
4. **Single binary story** — Rust produces a truly static binary. Go produces a static binary too, but CGo dependencies (whisper.cpp, robotgo) break static linking guarantees.
5. **Go's only advantage is Ollama** — The official Go client is nice, but `ollama-rs` is mature enough that this doesn't tip the scales.

## Risks and Concerns

1. **Whisper speculative decoding loss** — The biggest UX risk. Transcription will be ~2x slower unless using distil-large-v3 directly or a sidecar. Test with real users to validate acceptable latency.
2. **Status window** — Tkinter replacement in Rust is non-trivial. Consider running a separate lightweight process (e.g., a simple X11 window with `x11rb`) or using `egui` with X11 override-redirect.
3. **Whisper model loading time** — The Python version loads models in a background thread. Rust will need similar async initialization, which is straightforward with Tokio.
4. **whisper-rs maintenance** — The crate migrated from GitHub to Codeberg. Monitor for ecosystem fragmentation.
5. **Build complexity** — whisper-rs requires CUDA toolkit for GPU acceleration. CI/CD and developer setup need to account for this.

## Open Questions

1. Is the ~2x transcription latency increase acceptable with whisper.cpp (no speculative decoding), or should we benchmark distil-large-v3 standalone?
2. Should the status window be a separate process (simpler) or embedded in the main binary (single binary goal)?
3. Should we support Wayland in the rewrite (future-proofing), or keep X11-only?
4. What's the target for model loading time? The Python version takes ~10-15s to load Whisper on first run.
5. Should the rewrite maintain backward compatibility with the existing config.toml format?

## Related Research

- `thoughts/shared/research/2026-01-15-phase-1-research.md` — Original Phase 1 research
- `thoughts/shared/research/2026-01-30-whisper-long-form-transcription.md` — Whisper transcription analysis
- `thoughts/shared/project/2026-01-15-dictate-agent.md` — Original project spec (Rust-first design)
- `thoughts/shared/project/2026-01-15-dictate-agent-completion.md` — Completion report (Python MVP)

## Code References

- `dictate/main.py` — Central daemon orchestrator (471 LOC, signal handlers at lines 460-463)
- `dictate/audio.py` — Audio capture via parecord subprocess (94 LOC)
- `dictate/transcribe.py` — Whisper inference via HuggingFace (244 LOC, model loading at lines 73-131)
- `dictate/config.py` — TOML config with 9 dataclasses (232 LOC)
- `dictate/output.py` — xclip + xdotool clipboard-paste approach (92 LOC)
- `dictate/timer_executor.py` — Duration parser + systemd-run (274 LOC, most complex module)
- `dictate/status_window.py` — Tkinter overlay with threading (222 LOC)
- `dictate/history.py` — SQLite WAL-mode history (205 LOC)
- `dictate/local_executor.py` — Ollama client wrapper (182 LOC)
- `dictate/grammar.py` — Ollama grammar correction (106 LOC)
- `dictate/router.py` — Keyword prefix routing (80 LOC)
- `systemd/dictate-agent.service` — Already points to `target/release/dictate-agent` (Rust binary path)
