# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`dictate-agent` is a signal-driven Python daemon for voice dictation on Linux. It records audio via `parecord`, transcribes with Whisper (HuggingFace Transformers), runs a grammar-correction pass through Ollama, routes the text by first-word trigger, and either types it back with `xdotool` or dispatches to an executor (local Ollama model, systemd timer, etc.).

There is an archived Rust port in `src_rust_archive/` — ignore it unless the user explicitly asks about the rewrite. The active implementation is the `dictate/` Python package.

## Commands

The project uses a `.venv/` at the repo root; `scripts/run.sh` activates it and runs `python -m dictate.main`.

```bash
# Run the daemon (foreground, for development)
./scripts/run.sh

# Verify external dependencies without starting the daemon
.venv/bin/python -m dictate.main --check

# Trigger the running daemon (requires a live PID file)
./scripts/dictate-toggle     # SIGUSR1 — start/stop recording
./scripts/dictate-cancel     # SIGUSR2 — cancel without transcription

# Install as a systemd user service (see systemd/dictate-agent.service —
# note: the unit file points at a stale Rust binary path; edit ExecStart
# to ~/dictate_agent/scripts/run.sh before enabling)
systemctl --user daemon-reload && systemctl --user enable --now dictate-agent

# Lint (configured in pyproject.toml, line-length 100, target py310)
.venv/bin/ruff check dictate/
```

There is no test suite. `pyproject.toml` lists `pytest` as a dev extra but no tests exist yet.

## Architecture

Hub-and-spoke: `dictate/main.py` (`DictateAgent`) owns every component and drives the pipeline. Modules don't import each other — they only import config dataclasses and are wired together in `DictateAgent.__init__`.

### Signal-driven daemon loop

`main()` writes `~/.config/dictate-agent/dictate.pid`, installs `SIGUSR1`/`SIGUSR2`/`SIGINT` handlers, then sits in `signal.pause()`. All state transitions happen inside signal handlers — there is no event loop. The helper scripts (`dictate-toggle`, `dictate-cancel`) are thin `kill -USR1/-USR2` wrappers over that PID file; external keybindings bind to the scripts.

### Recording → output pipeline (`_stop_recording` in main.py)

```
parecord (audio.py, 16 kHz mono WAV)
  → Transcriber.transcribe()        whisper-large-v3-turbo via HF pipeline
  → Transcriber._apply_corrections() hardcoded fixups (Whisper mis-hears "Claude", slash commands)
  → GrammarCorrector.correct()       qwen3:0.6b via Ollama, fail-open
  → Router.route()                   first-word trigger → RouteType
  → _handle_route() dispatch:
      TYPE    → OutputHandler.type_text() via xdotool
      TIMER   → TimerExecutor (systemd-run transient unit + dunstify)
      LOCAL   → LocalExecutor (Ollama qwen3:14b) → xdotool
      EDIT    → not implemented
      COMMAND → not implemented
  → HistoryStore.commit()            SQLite row in ~/.local/share/dictate-agent/history.db
```

Around this, `main.py` pauses/resumes media via `playerctl` (state tracked in `~/.config/dictate-agent/media_was_playing`) and drives a `StatusWindow` overlay.

### Routing (`dictate/router.py`)

Default route is `TYPE` (verbatim typing). Triggers:
- First word `timer …` → `TIMER`
- First word `simple|easy|medium|hard …` → `LOCAL` (all four route identically; the word is treated as a prefix marker, not a difficulty signal)
- Prefix `edit:|fix:|change:|rewrite:|transform:` → `EDIT` (not yet wired up)

`GrammarCorrector` skips inputs under `min_words` (default 3) specifically so short utterances starting with these trigger words aren't reworded away before routing.

### Configuration (`dictate/config.py`)

Config is a tree of dataclasses loaded from `~/.config/dictate-agent/config.toml` (copy `config/config.example.toml`). Each subsection is re-constructed from `.get()` calls with the dataclass default as fallback, so missing keys are fine but unknown keys are silently ignored. When adding a new field: add it to the dataclass default and to the matching branch in `load_config()` — both edits are required.

### Transcription quirks

- Uses HF `pipeline("automatic-speech-recognition")` with `chunk_length_s` for long-form support, on SDPA attention (works on Blackwell / RTX 5080).
- `use_speculative_decoding` in config is honored as a flag but **intentionally skipped** — the assistant-model path is incompatible with the chunked pipeline (see comment in `transcribe.py`). Don't "fix" this without addressing that incompatibility.
- Models load in a background thread (`load_models_async`); `transcribe()` blocks on `model_loaded` until they're ready. First recording after daemon start will wait on the initial load.

### Executor conventions

Every executor (`local_executor.py`, `timer_executor.py`, `grammar.py`) exposes an `execute()`/`correct()` method that **never raises** — all errors are caught and returned as a result dataclass (`success`, `response`/`corrected`, `error`). `main.py` relies on this: it branches on `result.success` and never wraps executor calls in try/except. Preserve this contract when adding new executors.

Grammar correction is a **fail-open pipeline middleware**: on any failure (timeout, empty response, length-ratio sanity check outside 0.5–1.5) it returns the original text and the pipeline continues. Anything added between transcription and routing should follow the same pattern.

### Dependency checking

Each module with external deps exports `check_*_dependencies() -> list[tuple[str, str]]` returning `(missing_cmd, install_hint)` pairs. `main.check_all_dependencies()` aggregates them and `--check` reports them. New modules with subprocess dependencies should add a checker and wire it in.

## Further reading

`.claude/skills/dictate-agent-developer/SKILL.md` is the detailed developer guide — it covers the executor pattern, how to add a new route type (the 3 files that need to change), subprocess conventions per tool, notification icon conventions, and specs for the unimplemented EDIT / COMMAND / TTS features. Read it before non-trivial changes.
