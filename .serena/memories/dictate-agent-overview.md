# Dictate Agent Project Overview

## Project Purpose
Voice-activated AI assistant daemon that captures audio via keybind, transcribes speech using Whisper, routes requests to Claude models via local Ollama, and types responses into active windows.

## Key Keybindings
- **`$mod+n`**: Start recording / Stop and transcribe
- **`$mod+Shift+n`**: Cancel recording (discard without transcription)

**Important**: The complete keybindings documentation is maintained as a living document in `~/PC_USE_DOCS/dictate-agent-keybindings.md`. This document is referenced in the home directory's CLAUDE.md.

## Architecture
Signal flow: `User presses $mod+n → SIGUSR1 → dictate-agent daemon → Audio capture → Whisper transcription → Ollama routing → Claude Code execution → Stream response → Type into window`

## Tech Stack
- **Language**: Python (MVP implementation)
- **Transcription**: Whisper (transformers)
- **Classification**: Ollama (qwen3:0.6b)
- **AI Models**: Claude Haiku/Sonnet/Opus via Claude Code CLI
- **Audio**: PipeWire via parec
- **Output**: xdotool (X11 keyboard simulation)
- **Config**: TOML

## Core Modules
- `main.py`: Daemon orchestration, signal handling
- `audio.py`: Audio recording via parec (PipeWire)
- `transcribe.py`: Whisper transcription
- `router.py`: Ollama classification + keyword fallback
- `executor.py`: Claude Code subprocess management
- `output.py`: Text typing via xdotool
- `notify.py`: Toast notifications via notify-send

## Development Commands
```bash
# Setup
python -m venv .venv
source .venv/bin/activate
pip install -e .

# Run daemon
./scripts/run.sh

# Run via Python
python -m dictate.main

# Toggle recording
./scripts/dictate-toggle

# Tests
pytest
```

## Configuration
Location: `~/.config/dictate-agent/config.toml`

Key settings:
- `whisper.model`: Transcription model (default: `openai/whisper-large-v3-turbo`)
- `router.ollama_model`: Classification model (default: `qwen3:0.6b`)
- `output.auto_type`: Enable automatic typing (default: `true`)

## Important Notes
- **X11 only**: Requires X11, won't work on Wayland (due to xdotool dependency)
- **Signal handling**: SIGUSR1 (toggle), SIGUSR2 (cancel), SIGINT/SIGTERM (shutdown)
- **PID location**: `~/.config/dictate-agent/dictate.pid`
- **Service**: Systemd service available at `systemd/dictate-agent.service`

## Status
Python MVP implementation (original spec planned Rust). Active and functional for voice-to-Claude routing with local model classification.
