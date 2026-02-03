# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Dictate Agent** is a voice-activated AI assistant daemon that:
- Captures audio via keybind (`$mod+n`)
- Transcribes speech using Whisper
- Routes requests intelligently to Claude models (Haiku/Sonnet/Opus) via local Ollama
- Executes tasks through Claude Code CLI
- Types responses into active windows
- Provides toast notifications for status

**Status**: Python implementation (MVP). Original spec planned Rust, but Python was implemented for faster iteration.

## Architecture

### Signal Flow
```
User presses $mod+n → SIGUSR1 → dictate-agent daemon
→ Audio capture → Whisper transcription → Ollama routing
→ Claude Code execution → Stream response → Type into window
```

### Core Modules

| Module | Purpose | Key Classes |
|--------|---------|-------------|
| `main.py` | Daemon orchestration, signal handling | `DictateAgent` |
| `audio.py` | Audio recording via parec (PipeWire) | `AudioCapture` |
| `transcribe.py` | Whisper transcription (transformers) | N/A |
| `router.py` | Ollama classification + keyword fallback | `Router`, `RouteType` |
| `executor.py` | Claude Code subprocess management | `ClaudeExecutor`, `ExecutionResult` |
| `local_executor.py` | Ollama local inference (without Claude) | `LocalExecutor` |
| `timer_executor.py` | Timer via systemd-run transient timers | `TimerExecutor` |
| `output.py` | Text typing via xdotool | N/A |
| `notify.py` | Toast notifications via notify-send | N/A |
| `config.py` | TOML configuration loading | N/A |

### Routing Logic

**Primary**: Ollama (qwen3:0.6b) classifies requests into:
- `HAIKU` - Simple questions, quick lookups
- `SONNET` - Normal tasks, typical complexity
- `OPUS` - Complex analysis, architecture decisions
- `EDIT` - Text transformation (not implemented)
- `COMMAND` - System commands (not implemented)
- `TIMER` - Set a timer with desktop notification + sound
- `SIMPLE` - Trigger word for local Ollama inference (no Claude)

**Fallback**: If Ollama unavailable, keyword-based routing:
- Explicit keywords: "easy"→Haiku, "hard"→Opus
- Word count: <20→Haiku, >100→Opus
- Code terms bump to Sonnet minimum
- Default: Sonnet

## Development Commands

### Setup
```bash
# Create virtual environment
python -m venv .venv
source .venv/bin/activate

# Install dependencies
pip install -e .

# Copy and configure
mkdir -p ~/.config/dictate-agent
cp config/config.example.toml ~/.config/dictate-agent/config.toml

# Verify dependencies
python -m dictate.main  # Will check for parec, xdotool, notify-send, claude
```

### Running

```bash
# Start daemon directly
./scripts/run.sh

# Or via Python module
source .venv/bin/activate
python -m dictate.main

# Toggle recording (send SIGUSR1)
./scripts/dictate-toggle

# Stop daemon (send SIGINT)
kill $(cat ~/.config/dictate-agent/dictate.pid)
```

### Testing
```bash
# Run all tests
pytest

# Test individual components (examples, not implemented)
python -c "from dictate.router import Router; print(Router().route('what is Python'))"
```

## Configuration

Config location: `~/.config/dictate-agent/config.toml`

Key settings:
- `whisper.model`: Transcription model (default: `openai/whisper-large-v3-turbo`)
- `whisper.use_speculative_decoding`: 2x speedup (default: `true`)
- `router.ollama_model`: Classification model (default: `qwen3:0.6b`)
- `router.default_model`: Fallback when ambiguous (default: `sonnet`)
- `output.auto_type`: Enable automatic typing (default: `true`)

## Dependencies

**System** (checked via `check_all_dependencies()`):
- `parec` - PipeWire/PulseAudio recording
- `xdotool` - X11 keyboard simulation
- `notify-send` - Desktop notifications
- `claude` - Claude Code CLI
- `ollama` - Local LLM runtime (optional, for routing)

**Python** (from `pyproject.toml`):
- `torch`, `transformers` - Whisper transcription
- `accelerate`, `optimum` - Model optimization
- `ollama` - Ollama client library
- `tomli` - TOML config parsing

## Key Implementation Details

### Media Playback Handling
The agent detects playing media via `playerctl status` and pauses it during transcription/response, resuming afterward. This prevents audio conflicts.

### Speculative Decoding
Whisper uses assistant model (`distil-whisper/distil-large-v3`) for 2x speedup during transcription. Configured in `config.toml`.

### Claude Code Stream Parsing
Responses are NDJSON streams parsed line-by-line. Text deltas are extracted and typed character-by-character.

### Signal Handling
- `SIGUSR1`: Toggle recording (start/stop)
- `SIGUSR2`: Cancel recording (discard without transcription)
- `SIGINT`: Graceful shutdown
- `SIGTERM`: Graceful shutdown

Signals must be sent to PID stored in `~/.config/dictate-agent/dictate.pid`.

### Local Ollama Mode
Trigger word "simple" bypasses Claude entirely and uses local Ollama (qwen3:0.6b) for inference. Useful for offline or cost-free operation.

## i3wm Integration

Add to i3 config:
```
bindsym $mod+n exec ~/dictate_agent/scripts/dictate-toggle
bindsym $mod+Shift+n exec ~/dictate_agent/scripts/dictate-cancel
```

| Keybind | Action |
|---------|--------|
| `$mod+n` | Start recording / Stop and transcribe |
| `$mod+Shift+n` | Cancel recording (discard without transcription) |

Systemd service provided at `systemd/dictate-agent.service` for auto-start.

## Known Limitations

1. **No TTS**: General questions not spoken aloud (planned Phase 3)
2. **No edit mode**: Text transformation detection exists but not implemented
3. **No keyboard commands**: System command detection exists but not implemented
4. **No silence detection**: Recording continues until manual stop
5. **X11 only**: xdotool requires X11, won't work on Wayland

## Project Context

This was originally planned as a Rust project for single-binary deployment, but Python was chosen for MVP to leverage existing PyTorch ecosystem. See `thoughts/shared/project/2026-01-15-dictate-agent.md` for full original specification.

The completion report at `thoughts/shared/project/2026-01-15-dictate-agent-completion.md` documents what was built vs. planned.

## Troubleshooting

### "No module named 'dictate'"
Run from project root with `python -m dictate.main`, not `python dictate/main.py`.

### "parec not found"
Install PipeWire/PulseAudio: `sudo pacman -S pipewire-pulse`

### Ollama classification timing out
Check Ollama is running: `systemctl --user status ollama`
Pull model: `ollama pull qwen3:0.6b`

### Text not typing into window
Verify xdotool: `xdotool version`
Check X11 (not Wayland): `echo $XDG_SESSION_TYPE`

### Claude Code not found
Install Claude Code CLI and ensure `claude` is in PATH.
