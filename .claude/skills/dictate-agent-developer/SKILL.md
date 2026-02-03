---
name: dictate-agent-developer
description: "Guide development of the dictate-agent voice assistant daemon. Use when modifying, extending, or debugging the dictate_agent Python codebase — including: (1) Adding new features or route types, (2) Creating new executor modules, (3) Modifying audio capture, transcription, or output, (4) Updating configuration, (5) Adding new external tool integrations, (6) Implementing planned features (EDIT mode, COMMAND mode, TTS), (7) Debugging signal handling, subprocess, or daemon issues. Triggers: dictate, voice, audio, transcribe, whisper, route, executor, xdotool, parecord, notify-send."
---

# Dictate Agent Developer Guide

## Architecture

Hub-and-spoke: `main.py` orchestrates all modules. Modules are independent — they don't import each other (except `transcribe.py` and `router.py` importing config dataclasses).

```
main.py (hub) ──┬── audio.py          (parecord subprocess)
                ├── transcribe.py     (Whisper pipeline, torch)
                ├── grammar.py        (ollama pipeline middleware)
                ├── router.py         (trigger-word classification)
                ├── executor.py       (claude CLI subprocess)
                ├── local_executor.py (ollama Python client)
                ├── timer_executor.py (systemd-run subprocess)
                ├── output.py         (xdotool subprocess)
                ├── notify.py         (notify-send subprocess)
                └── config.py         (TOML loading, dataclasses)
```

### Signal Flow

```
$mod+n → scripts/dictate-toggle → kill -USR1 <pid> → DictateAgent.toggle()
$mod+Shift+n → scripts/dictate-cancel → kill -USR2 <pid> → DictateAgent.cancel()
```

PID stored at `~/.config/dictate-agent/dictate.pid`. Media state tracked via `~/.config/dictate-agent/media_was_playing` file.

### Data Flow

```
toggle() → _start_recording() / _stop_recording()
→ AudioCapture → parecord WAV → Transcriber (Whisper pipeline)
→ GrammarCorrector.correct() (fail-open pipeline middleware)
→ Router.route() → RouteType enum → _handle_route() dispatch:
    TYPE    → OutputHandler.type_text()
    HAIKU/SONNET/OPUS → ClaudeExecutor.execute()
    LOCAL   → LocalExecutor.execute()
    TIMER   → TimerExecutor.execute()
    EDIT    → (not implemented)
    COMMAND → (not implemented)
```

## Core Patterns

### 1. Adding a New Route Type

Requires changes in exactly **3 files**. See [references/adding-route-types.md](references/adding-route-types.md) for step-by-step.

### 2. Adding a New Executor

Follow the executor pattern: dataclass result, `execute()` method, never raise exceptions. See [references/adding-executors.md](references/adding-executors.md).

### 3. Adding a Pipeline Step

Pipeline steps transform text between transcription and routing (e.g., grammar correction). They are **fail-open middleware** — on failure they return the original text, never blocking the pipeline. See [references/adding-pipeline-steps.md](references/adding-pipeline-steps.md).

### 4. Configuration Changes

Dataclass defaults → TOML loading with `.get()` → constructor injection. See [references/config-patterns.md](references/config-patterns.md).

### 5. Subprocess Conventions

Each external tool has specific patterns:

| Tool | Module | Invocation | Stop Method |
|------|--------|------------|-------------|
| `parecord` | audio.py | `Popen`, long-running | `send_signal(SIGINT)` + wait |
| `xdotool` | output.py | `run`, one-shot | N/A |
| `notify-send` | notify.py | `run`, fire-and-forget | N/A (silent failure) |
| `claude` | executor.py | `run`, 120s timeout | Timeout kills |
| `ollama` | local_executor.py | Python client library | Client timeout |
| `ollama` | grammar.py | Python client library | Client timeout (10s) |
| `systemd-run` | timer_executor.py | `run`, 5s timeout | N/A |
| `playerctl` | main.py | `run`, fire-and-forget | N/A (silent failure) |

### 6. Notification Icons

Use these consistently:

| Context | Icon |
|---------|------|
| Recording | `audio-input-microphone` |
| Processing | `emblem-synchronizing` |
| Success | `emblem-ok-symbolic` |
| Error | `dialog-error` |
| Warning | `dialog-warning` |
| Cancelled | `dialog-cancel` |
| Timer | `alarm-symbolic` |

### 7. Dependency Checking

Every module with external deps exports `check_*_dependencies() -> list[tuple[str, str]]` returning `(command, package)` pairs. Aggregated in `main.py:check_all_dependencies()`.

### 8. Error Handling

Executors return result dataclasses with `success: bool, response: str, error: Optional[str]`. They **never raise exceptions** — all errors are caught and returned as result objects. Non-critical operations (notifications, media control) use `try/except: pass`.

## Module Quick Reference

| Module | Key Class/Function | Lines |
|--------|--------------------|-------|
| `main.py` | `DictateAgent`, `main()` | ~397 |
| `audio.py` | `AudioCapture` (dataclass) | ~93 |
| `transcribe.py` | `Transcriber`, `TranscriptionResult` | ~234 |
| `grammar.py` | `GrammarCorrector`, `GrammarResult` | ~107 |
| `router.py` | `Router`, `RouteType` (enum), `RouteResult` | ~91 |
| `executor.py` | `ClaudeExecutor`, `ExecutionResult` | ~140 |
| `local_executor.py` | `LocalExecutor`, `ExecutionResult` | ~156 |
| `timer_executor.py` | `TimerExecutor`, `TimerResult` | ~241 |
| `output.py` | `OutputHandler` (dataclass) | ~55 |
| `notify.py` | `Notifier` (dataclass) | ~130 |
| `config.py` | `Config` + 7 nested dataclasses, `load_config()` | ~216 |

## Planned Features

Original spec defined 6 phases. Only 3 were implemented. See [references/planned-features.md](references/planned-features.md) for detailed specs of:
- **EDIT mode**: Voice-triggered text transformation via clipboard capture
- **COMMAND mode**: Voice-triggered keyboard shortcuts and shell commands
- **TTS**: Text-to-speech for general knowledge answers

## Detailed References

- **[references/architecture.md](references/architecture.md)** — Read when understanding module relationships, signal handling, or media state tracking
- **[references/adding-route-types.md](references/adding-route-types.md)** — Read when adding a new route type
- **[references/adding-executors.md](references/adding-executors.md)** — Read when creating a new executor module
- **[references/adding-pipeline-steps.md](references/adding-pipeline-steps.md)** — Read when adding text transformation between transcription and routing
- **[references/config-patterns.md](references/config-patterns.md)** — Read when adding or modifying configuration
- **[references/planned-features.md](references/planned-features.md)** — Read when implementing EDIT, COMMAND, or TTS features
