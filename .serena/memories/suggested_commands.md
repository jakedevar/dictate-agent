# Dictate Agent: Suggested Commands

## Quick Start
```bash
# Activate venv and run daemon
source .venv/bin/activate
./scripts/run.sh

# Or directly
python -m dictate.main
```

## Development Workflow

### Setup (first time)
```bash
python -m venv .venv
source .venv/bin/activate
pip install -e .
mkdir -p ~/.config/dictate-agent
cp config/config.example.toml ~/.config/dictate-agent/config.toml
```

### Testing
```bash
# Run all tests
pytest

# Run specific test module
pytest tests/test_audio.py
```

### Code Quality
```bash
# Format (if configured)
black dictate/

# Linting (if configured)
flake8 dictate/
```

## Daemon Control

### Start
```bash
./scripts/run.sh
# or
python -m dictate.main
```

### Toggle Recording (equivalent to `$mod+n`)
```bash
./scripts/dictate-toggle
# or
kill -USR1 $(cat ~/.config/dictate-agent/dictate.pid)
```

### Cancel Recording (equivalent to `$mod+Shift+n`)
```bash
./scripts/dictate-cancel
# or
kill -USR2 $(cat ~/.config/dictate-agent/dictate.pid)
```

### Graceful Shutdown
```bash
kill $(cat ~/.config/dictate-agent/dictate.pid)
# or
kill -INT $(cat ~/.config/dictate-agent/dictate.pid)
```

## Systemd Service

```bash
# Enable auto-start
systemctl --user enable dictate-agent

# Start service
systemctl --user start dictate-agent

# Stop service
systemctl --user stop dictate-agent

# View logs
journalctl --user -u dictate-agent -f

# Check status
systemctl --user status dictate-agent
```

## Configuration Management

```bash
# View current config
cat ~/.config/dictate-agent/config.toml

# Edit config
nvim ~/.config/dictate-agent/config.toml

# Copy fresh example
cp config/config.example.toml ~/.config/dictate-agent/config.toml
```

## Troubleshooting Commands

```bash
# Check parec availability
parec --version

# Check xdotool availability
xdotool version

# Check Ollama status
systemctl --user status ollama

# Pull Ollama model
ollama pull qwen3:0.6b

# Check Claude Code installation
which claude

# View daemon PID
cat ~/.config/dictate-agent/dictate.pid

# View daemon logs
tail -f ~/.config/dictate-agent/dictate.log (if logging configured)
```

## Dependencies Check

The daemon automatically checks all dependencies on startup via `check_all_dependencies()`:
- `parec` (PipeWire recording)
- `xdotool` (X11 keyboard simulation)
- `notify-send` (notifications)
- `claude` (Claude Code CLI)
- `ollama` (optional, for routing)

If missing, install via:
```bash
p pipewire-pulse xdotool libnotify  # System packages
# claude via: https://github.com/anthropics/claude-code
# ollama via: https://ollama.ai
```

## Project Navigation

```bash
cd ~/dictate_agent          # Project root
cd dictate/                 # Source code
cd config/                  # Configuration files
cd scripts/                 # Utility scripts
cd systemd/                 # Systemd service
cd tests/                   # Test files
```
