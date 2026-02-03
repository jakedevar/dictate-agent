---
date: 2026-02-02T12:00:00-08:00
researcher: Claude (Opus 4.5)
git_commit: 5f4033f849cc53630b5c92024bf046ee80ea2d9c
branch: master
repository: dictate_agent
topic: "Dictate Agent Codebase Research & Skill Feasibility Analysis"
tags: [research, codebase, skill, dictate-agent, architecture, python, voice-assistant]
status: complete
last_updated: 2026-02-02
last_updated_by: Claude (Opus 4.5)
---

# Research: Dictate Agent Codebase Research & Skill Feasibility Analysis

**Date**: 2026-02-02T12:00:00-08:00
**Researcher**: Claude (Opus 4.5)
**Git Commit**: 5f4033f849cc53630b5c92024bf046ee80ea2d9c
**Branch**: master
**Repository**: dictate_agent

## Research Question

Comprehensive codebase documentation and assessment of whether a Claude Code skill can be created to help future agents modify or extend this program.

## Summary

The dictate_agent is a Python voice-activated AI assistant daemon with a clean, modular architecture of 11 Python source files. Each module is standalone with minimal cross-dependencies — the `main.py` orchestrator imports everything, but modules rarely import each other. The codebase uses consistent patterns (dataclass-based config, subprocess wrappers, signal-based control, notify-send notifications) that are highly documentable.

**Skill feasibility: Yes, and strongly recommended.** The codebase has several properties that make a skill highly valuable:

1. **Consistent patterns** that must be followed for new modules (dataclass config, dependency checking, notification conventions)
2. **Non-obvious integration points** (signal handling, PID file system, media playback state tracking)
3. **Planned but unimplemented features** that follow documented patterns (EDIT mode, COMMAND mode, TTS)
4. **External tool conventions** (parecord, xdotool, notify-send, claude CLI, ollama) that require specific subprocess patterns
5. **Routing architecture** that new route types must integrate with

## Detailed Findings

### Module Architecture

The codebase follows a hub-and-spoke pattern with `main.py` as the orchestrator:

```
                        config.py
                           │
    ┌──────────────────────┼──────────────────────┐
    │                      │                      │
transcribe.py          router.py              main.py (hub)
    │                      │                      │
    │                      │          ┌───────────┼───────────────┐
    │                      │          │           │               │
    │                      │     audio.py    notify.py       output.py
    │                      │          │           │               │
    │                      │          │           │               │
    │                      │     executor.py  local_executor.py  timer_executor.py
    └──────────────────────┴──────────┴───────────┴───────────────┘
```

**Key observation**: Only `transcribe.py` and `router.py` import from `config.py` directly. All other modules receive configuration through constructor parameters from `main.py`. This means new modules should follow the same pattern: accept config values in `__init__`, don't import config directly.

### File Inventory (11 Python modules)

| Module | Lines | Purpose | External Tools |
|--------|-------|---------|----------------|
| `main.py` | ~374 | Orchestration, signal handling | playerctl |
| `audio.py` | ~93 | Audio recording | parecord |
| `transcribe.py` | ~234 | Whisper transcription | torch, transformers |
| `router.py` | ~91 | Request classification | (none) |
| `executor.py` | ~140 | Claude Code execution | claude CLI |
| `local_executor.py` | ~156 | Ollama inference | ollama |
| `timer_executor.py` | ~241 | Timer creation | systemd-run, notify-send |
| `output.py` | ~55 | Text typing | xdotool |
| `notify.py` | ~130 | Notifications | notify-send |
| `config.py` | ~194 | TOML config loading | (none) |
| `__init__.py` | ~4 | Package version | (none) |

### Pattern Catalog

#### 1. New Module Pattern
Every module follows this structure:
- Imports at top (stdlib, then third-party, then internal)
- `@dataclass` for result types and state classes
- Main class with `__init__` accepting config values
- `check_*_dependencies()` function returning `list[tuple[str, str]]`
- Type hints on all functions
- `pathlib.Path` for file paths

#### 2. New Route Type Pattern
Adding a route type requires changes in exactly 3 files:
1. `router.py`: Add enum value to `RouteType`, add trigger detection in `route()` method
2. `main.py`: Add handler case in `_handle_route()` method
3. `config.py`: Add config dataclass if configurable settings needed

#### 3. New Executor Pattern
Each executor follows:
- `@dataclass` result class with `success: bool`, `response: str`, `error: Optional[str]`
- `execute(prompt: str) -> ResultType` method
- Error handling returns result objects (never raises)
- Print statements for status/debugging

#### 4. Notification Convention
Icon names used consistently:
- `audio-input-microphone` — recording
- `emblem-synchronizing` — processing
- `emblem-ok-symbolic` — success
- `dialog-error` — error
- `dialog-warning` — warning
- `dialog-cancel` — cancelled
- `alarm-symbolic` — timer

#### 5. Configuration Convention
- Add `@dataclass` to `config.py` with all fields having defaults
- Add to parent `Config` dataclass with `field(default_factory=NewConfig)`
- Add TOML loading block in `load_config()` using `.get()` with defaults
- Add section to `config/config.example.toml` with comments

### Unimplemented Features (from original spec)

These are detected by routing but not executed:

| Feature | Route Type | Detection | Status |
|---------|-----------|-----------|--------|
| Text editing | EDIT | Prefix triggers ("edit:", "fix:") | Prints "not yet implemented" |
| Keyboard commands | COMMAND | Prints "not yet implemented" | Prints "not yet implemented" |
| TTS | N/A | Not started | Phase 3 in original spec |
| Silence detection | N/A | Not started | Open question in spec |

### External Dependencies

| Tool | Used By | How Invoked | Fallback |
|------|---------|-------------|----------|
| `parecord` | audio.py | `subprocess.Popen` | None (required) |
| `xdotool` | output.py | `subprocess.run` | None (required) |
| `notify-send` | notify.py | `subprocess.run` | Silent failure |
| `claude` | executor.py | `subprocess.run` | Returns error result |
| `ollama` | local_executor.py | Python `ollama` library | Returns error result |
| `playerctl` | main.py | `subprocess.run` | Silent failure |
| `systemd-run` | timer_executor.py | `subprocess.run` | Returns error result |

### Historical Context

| Document | Key Insight |
|----------|-------------|
| `thoughts/shared/project/2026-01-15-dictate-agent.md` | Original Rust spec with all planned features (TTS, edit mode, keyboard commands) |
| `thoughts/shared/project/2026-01-15-dictate-agent-completion.md` | 3/6 phases completed, Python chosen over Rust |
| `thoughts/shared/research/2026-01-23-interruption-keybind.md` | SIGUSR2 cancel feature — now implemented |
| `thoughts/shared/research/2026-01-30-whisper-long-form-transcription.md` | Pipeline migration for >30s audio, speculative decoding incompatibility |
| `thoughts/shared/handoffs/general/2026-01-21_audio-recording-cutoff-debug.md` | parecord buffer issues, pw-record as potential fix |

## Skill Feasibility Analysis

### Why a Skill Would Be Valuable

1. **Non-obvious module integration**: New agents won't know about the hub-and-spoke pattern, the 3-file route type pattern, or the dependency checking convention without being told.

2. **Subprocess conventions**: Each external tool has specific invocation patterns (SIGINT for parecord, `--clearmodifiers` for xdotool, synchronous hints for notify-send). These are easy to get wrong.

3. **Signal architecture**: The PID file, signal handler registration, and media state file system are non-obvious. A new feature that needs to interact with the daemon lifecycle needs to understand this.

4. **Configuration system**: The config loading uses a specific pattern with `.get()` and dataclass defaults that must be followed exactly.

5. **Planned features exist as specs**: The original Rust spec documents detailed behavior for EDIT, COMMAND, and TTS modes. A skill can reference these specifications.

### Recommended Skill Structure

```
dictate-agent-developer/
├── SKILL.md                    # Core patterns, module conventions, integration guide
└── references/
    ├── architecture.md         # Module map, data flow, signal architecture
    ├── adding-route-types.md   # Step-by-step for new route types
    ├── adding-executors.md     # Step-by-step for new executor modules
    ├── config-patterns.md      # Configuration system conventions
    └── planned-features.md     # Specs for EDIT, COMMAND, TTS from original design
```

### What the SKILL.md Should Cover

1. **Architecture overview** — hub-and-spoke, module responsibilities
2. **Adding a new route type** — the 3-file pattern (router.py, main.py, config.py)
3. **Adding a new executor** — result dataclass, execute method, error handling
4. **Configuration conventions** — dataclass defaults, TOML loading, example config
5. **Subprocess patterns** — how each external tool is called
6. **Signal handling** — daemon lifecycle, PID file, media state
7. **Notification conventions** — icon names, timeout values, replacement hints
8. **Dependency checking** — `check_*_dependencies()` pattern
9. **References to planned features** — where to find specs for unimplemented modes

### Estimated Skill Complexity

- **SKILL.md**: ~200-300 lines (medium freedom — patterns guide but don't constrain)
- **References**: ~400-500 lines across 5 files
- **Scripts**: None needed (the codebase patterns are text-based, not script-automatable)
- **Complexity**: Medium — mostly procedural knowledge and conventions

## Code References

- `dictate/main.py:38-74` — DictateAgent.__init__ (component wiring)
- `dictate/main.py:173-233` — _handle_route (route type dispatch)
- `dictate/main.py:358-371` — Signal handler registration
- `dictate/router.py:19-29` — RouteType enum
- `dictate/router.py:48-90` — Route classification logic
- `dictate/config.py:98-107` — Top-level Config dataclass
- `dictate/config.py:110-194` — load_config() TOML loading
- `dictate/executor.py:10-16` — ExecutionResult pattern
- `dictate/notify.py:15-54` — Notification dispatch pattern
- `dictate/audio.py:86-93` — Dependency checking pattern

## Architecture Documentation

### Data Flow
```
$mod+n → scripts/dictate-toggle → kill -USR1 <pid>
→ DictateAgent.toggle() → _start_recording() or _stop_recording()
→ AudioCapture.start()/stop() → parecord subprocess
→ Transcriber.transcribe() → Whisper pipeline
→ Router.route() → RouteType enum
→ _handle_route() dispatches to:
  ├── OutputHandler.type_text() → xdotool (TYPE route)
  ├── ClaudeExecutor.execute() → claude CLI (HAIKU/SONNET/OPUS)
  ├── LocalExecutor.execute() → ollama (LOCAL route)
  ├── TimerExecutor.execute() → systemd-run (TIMER route)
  ├── (not implemented) → EDIT route
  └── (not implemented) → COMMAND route
→ Notifier throughout for status updates
```

### Configuration Hierarchy
```
~/.config/dictate-agent/config.toml
→ config.py:load_config()
→ Config dataclass (6 nested config dataclasses)
→ main.py passes values to component constructors
```

## Historical Context (from thoughts/)

- `thoughts/shared/project/2026-01-15-dictate-agent.md` — Full original specification with Rust-based architecture, 6 implementation phases, and detailed feature specs for all planned modes
- `thoughts/shared/project/2026-01-15-dictate-agent-completion.md` — Completion report documenting 3/6 phases built, pivot from Rust to Python, 14 passing tests
- `thoughts/shared/research/2026-01-23-interruption-keybind.md` — Research leading to SIGUSR2 cancel implementation
- `thoughts/shared/research/2026-01-30-whisper-long-form-transcription.md` — Pipeline migration solving 30-second truncation bug
- `thoughts/shared/handoffs/general/2026-01-21_13-35-03_audio-recording-cutoff-debug.md` — Investigation of parecord buffer issues, pw-record alternative identified

## Open Questions

1. Should the skill include the original Rust spec's feature details for EDIT/COMMAND/TTS, or should those be re-specified for the Python implementation?
2. Should the skill cover testing patterns? (Currently no tests exist for the Python implementation)
3. Should the skill address the pw-record migration from the audio cutoff debug handoff?
