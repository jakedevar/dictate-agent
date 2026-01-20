---
date: 2026-01-15T19:30:00-06:00
author: Jake Devar
status: draft
project_type: cli
primary_language: rust
tags: [project, specification, voice, ai, claude-code, tts, dictation, rust, ollama]
---

# Project: Dictate Agent

## Vision

A voice-activated AI assistant that lives on your desktop, triggered by a keybind, that intelligently routes requests to the appropriate Claude model (Haiku/Sonnet/Opus) based on complexity, streams responses as typed text, and speaks answers to general questions aloud using local TTS. Built in Rust for a single-binary, instant-startup experience.

## Problem Statement

Voice-to-text dictation is useful, but it's passive—it just transcribes. You already have soupawhisper for that. What you want is an **active agent** that can hear your request, decide how much compute it needs, execute it via Claude Code, and return results seamlessly into your workflow.

### Who Has This Problem

Power users with voice dictation who want to extend it beyond transcription into AI-assisted task execution without leaving their keyboard-driven workflow.

### Current Solutions & Their Gaps

| Solution | Gap |
|----------|-----|
| SoupaWhisper | Only transcribes, no AI processing |
| Claude Code interactive | Requires terminal focus, not voice-activated |
| Voice assistants (Alexa, etc.) | Cloud-only, no local compute, no code context |

## Goals & Non-Goals

### Goals
- Single keybind (`$mod+m`) triggers voice recording
- Intelligent model routing via local Ollama model (qwen3:0.6b)
- Streaming response typed into active window via enigo
- Visual feedback via toast notifications (recording → processing → responding)
- TTS playback for general knowledge questions
- Stateless operation (each request is independent)
- Low latency end-to-end experience
- **Single binary deployment** - no Python, no venv, no pip

### Non-Goals (Explicitly Out of Scope)
- Conversation memory/context between requests (future enhancement)
- Custom wake word detection (use keybind instead)
- Multi-turn dialogue in a single activation
- GUI configuration interface
- Cross-platform support (Linux/i3 only)

## Core Features (MVP)

### 1. Voice Capture
**What**: Toggle-based recording using cpal (ALSA backend, PipeWire compatible)
**Why**: Native Rust audio capture, no system tool dependency
**Acceptance Criteria**:
- [ ] `$mod+m` starts recording with visual indicator
- [ ] Second `$mod+m` or silence detection stops recording
- [ ] Audio captured as 16kHz mono f32 samples for Whisper

### 2. Speech-to-Text
**What**: Local transcription using candle-whisper (pure Rust, CUDA-accelerated)
**Why**: Single binary, no Python dependency, 35-47% faster than PyTorch
**Acceptance Criteria**:
- [ ] Transcription completes in <2 seconds for typical requests
- [ ] Handles your voice accurately with base.en model
- [ ] Returns clean text without hallucinated punctuation
- [ ] GPU acceleration via CUDA on RTX 5080

### 3. Model Router (Ollama-Powered)
**What**: Local LLM classifier using qwen3:0.6b via Ollama to determine Haiku/Sonnet/Opus
**Why**: Zero API latency, fully local, can understand nuance better than keyword matching
**Acceptance Criteria**:
- [ ] Routes to Ollama's qwen3:0.6b for classification
- [ ] Returns one of: haiku, sonnet, opus, edit, command
- [ ] Falls back to keyword matching if Ollama unavailable
- [ ] Classification completes in <500ms

**Router Prompt**:
```
You are a request classifier for a voice assistant. Classify the user's request into exactly one category.

Categories:
- HAIKU: Simple factual questions, quick lookups, one-liner answers
- SONNET: Normal coding tasks, moderate complexity, typical development work
- OPUS: Complex analysis, architecture decisions, multi-file refactoring, deep reasoning
- EDIT: Text transformation requests starting with "edit:", "fix:", "change:", "rewrite:"
- COMMAND: System commands like "close window", "volume up", "workspace 2", "save", "undo"

User request: "{transcription}"

Respond with ONLY the category name in caps, nothing else.
```

**Fallback Keywords** (if Ollama unavailable):
- "easy", "quick", "simple" → HAIKU
- "hard", "complex", "analyze" → OPUS
- "edit:", "fix:", "change:" prefix → EDIT
- Known command phrases → COMMAND
- Default → SONNET

### 4. Claude Code Execution
**What**: Headless Claude Code invocation with streaming output via tokio subprocess
**Why**: Full agent capabilities, tool access, your existing config
**Acceptance Criteria**:
- [ ] Executes via `claude -p "prompt" --model <model> --output-format stream-json`
- [ ] Parses NDJSON stream in real-time using serde
- [ ] Respects existing Claude Code permissions and MCP config
- [ ] Handles errors gracefully with notification

### 5. Response Output
**What**: Stream response as typed text via enigo (x11rb backend)
**Why**: Pure Rust, no xdotool system dependency, reliable X11 injection
**Acceptance Criteria**:
- [ ] Characters appear as they stream from Claude
- [ ] Typing speed feels natural (configurable delay)
- [ ] Special characters and newlines handled correctly
- [ ] Can be interrupted (future: escape key)

### 6. Toast Notifications
**What**: Visual status indicator via notify-rust + dunst
**Why**: Native Rust, direct D-Bus, no subprocess calls
**Acceptance Criteria**:
- [ ] Recording: Orange/yellow indicator "Listening..."
- [ ] Processing: Blue indicator "Thinking..." with model name
- [ ] Responding: Green indicator "Responding..."
- [ ] Error: Red indicator with brief message
- [ ] Uses dunst hints for positioning (top center)

### 7. Text-to-Speech (General Questions)
**What**: Speak responses aloud using piper-rs (ONNX Runtime)
**Why**: Rust native, same Piper models, no system TTS dependency
**Acceptance Criteria**:
- [ ] Detects "general question" vs "task/code request"
- [ ] Uses Piper voice models via ort crate
- [ ] Speaks while also typing (parallel output via tokio)
- [ ] Can be disabled via config or keyword ("don't speak")

### 8. Text Editing Mode
**What**: Voice commands that manipulate text in the active input field via LLM
**Why**: Edit, transform, or fix text hands-free without switching context
**Acceptance Criteria**:
- [ ] Trigger phrase detection (e.g., "edit:", "fix:", "change:")
- [ ] Captures current field text via `Ctrl+A` → `Ctrl+C` (enigo)
- [ ] Sends text + command to Haiku for fast transformation
- [ ] Replaces field content with transformed result via `Ctrl+A` → paste/type
- [ ] Preserves cursor position when possible
- [ ] Toast shows "Editing..." state

**Example Commands**:
- "edit: remove all instances of purple"
- "fix: correct the grammar"
- "change: make this more formal"
- "edit: replace foo with bar"
- "fix: add proper punctuation"
- "change: summarize this in one sentence"

**Data Flow**:
```
1. User says "edit: fix the grammar"
2. Router (Ollama) classifies as EDIT
3. enigo sends Ctrl+A, Ctrl+C to capture field text
4. Read clipboard via arboard crate
5. Send to Haiku: "Fix the grammar in this text: <text>"
6. Receive transformed text
7. enigo sends Ctrl+A, then types new text
8. Toast: "Edited!" with preview
```

### 9. Keyboard Command Mode
**What**: Voice-triggered keyboard shortcuts and OS commands
**Why**: Full voice control of i3, applications, and system without touching keyboard
**Acceptance Criteria**:
- [ ] Router (Ollama) classifies as COMMAND
- [ ] Static mapping for common commands (no LLM needed)
- [ ] Executes via enigo for keyboard shortcuts
- [ ] Executes via std::process::Command for shell commands
- [ ] Sources command vocabulary from config

**Command Categories**:

| Category | Example Voice Commands | Action |
|----------|----------------------|--------|
| Window Management | "close window", "kill this" | `enigo key super+q` |
| Workspace | "workspace 1", "go to chrome" | `enigo key super+a` |
| Navigation | "focus left", "move window right" | `enigo key super+h/l` |
| Apps | "open terminal", "launch browser" | `enigo key super+Return` |
| Media | "play", "pause", "next song" | `playerctl play-pause/next` |
| Volume | "volume up", "mute" | `pactl set-sink-volume...` |
| Screenshot | "take screenshot" | `flameshot gui` |
| Common Hotkeys | "save", "undo", "copy", "paste" | `enigo key ctrl+s/z/c/v` |
| i3 | "reload config", "restart i3" | `enigo key super+shift+c/r` |

**Static Command Map** (loaded from TOML config):
```rust
lazy_static! {
    static ref KEYBOARD_COMMANDS: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        // Window management
        m.insert("close window", "super+q");
        m.insert("close this", "super+q");
        m.insert("kill window", "super+q");

        // Workspaces (your a/s/d/f/g layout)
        m.insert("workspace 1", "super+a");
        m.insert("workspace 2", "super+s");
        m.insert("workspace 3", "super+d");
        m.insert("workspace 4", "super+f");
        m.insert("workspace 5", "super+g");
        m.insert("go to chrome", "super+s");
        m.insert("go to spotify", "super+d");
        m.insert("go to terminal", "super+z");
        m.insert("go to files", "super+6");

        // Navigation
        m.insert("focus left", "super+h");
        m.insert("focus right", "super+l");
        m.insert("focus up", "super+k");
        m.insert("focus down", "super+j");

        // Common hotkeys
        m.insert("save", "ctrl+s");
        m.insert("undo", "ctrl+z");
        m.insert("redo", "ctrl+shift+z");
        m.insert("copy", "ctrl+c");
        m.insert("paste", "ctrl+v");
        m.insert("select all", "ctrl+a");
        m.insert("new tab", "ctrl+t");
        m.insert("close tab", "ctrl+w");

        // Screenshot
        m.insert("screenshot", "super+shift+0");
        m.insert("take screenshot", "super+shift+0");

        // i3 management
        m.insert("reload config", "super+shift+c");
        m.insert("restart i3", "super+shift+r");
        m
    };

    static ref SHELL_COMMANDS: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        // Media
        m.insert("play", "playerctl play-pause");
        m.insert("pause", "playerctl play-pause");
        m.insert("next song", "playerctl next");
        m.insert("previous song", "playerctl previous");

        // Volume
        m.insert("volume up", "pactl set-sink-volume @DEFAULT_SINK@ +5%");
        m.insert("volume down", "pactl set-sink-volume @DEFAULT_SINK@ -5%");
        m.insert("mute", "pactl set-sink-mute @DEFAULT_SINK@ toggle");
        m
    };
}
```

## Technology Stack

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Language | Rust | Single binary, fast startup, type safety, excellent async |
| Speech-to-Text | candle-whisper | Pure Rust, CUDA support, 35-47% faster inference |
| Model Router | Ollama (qwen3:0.6b) | Local LLM, <500ms classification, understands nuance |
| AI Backend | Claude Code CLI | Full agent capabilities, existing setup |
| Text-to-Speech | piper-rs + ort | ONNX inference, same Piper models |
| Audio Recording | cpal | ALSA backend, PipeWire compatible |
| Text Input | enigo | x11rb backend, no xdotool dependency |
| Clipboard | arboard | Cross-platform clipboard access |
| Notifications | notify-rust | Direct D-Bus to dunst |
| Async Runtime | tokio | Subprocess streaming, concurrent operations |
| Config | config + serde | TOML configuration files |
| Process Management | systemd user service | Reliable daemon management |

### Key Dependencies (Cargo.toml)

```toml
[package]
name = "dictate-agent"
version = "0.1.0"
edition = "2024"

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full", "process", "signal"] }

# Speech-to-text (Whisper)
candle-core = { version = "0.9", features = ["cuda"] }
candle-nn = { version = "0.9", features = ["cuda"] }
candle-transformers = { version = "0.9", features = ["cuda"] }
hf-hub = "0.4"
tokenizers = "0.21"

# Text-to-speech (Piper)
piper-rs = "0.1"
ort = { version = "2.0", features = ["cuda"] }

# Audio recording
cpal = "0.15"

# X11 automation (replaces xdotool)
enigo = "0.6"

# Clipboard
arboard = "3"

# Notifications
notify-rust = "4"

# Ollama client
ollama-rs = "0.2"

# JSON parsing for Claude Code output
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Configuration
config = "0.14"
toml = "0.8"

# Logging
tracing = "0.1"
tracing-subscriber = "0.3"

# Error handling
anyhow = "1"
thiserror = "2"

# Fuzzy matching for commands
strsim = "0.11"

[build-dependencies]
# For CUDA linking
cc = "1"
```

### System Dependencies

```bash
# Required system packages
sudo pacman -S alsa-lib cuda        # Audio + GPU
yay -S ollama                       # Local LLM runtime

# Ollama model (already installed)
ollama pull qwen3:0.6b              # 522MB, ultra-fast classification

# Piper voice model (download separately)
mkdir -p ~/.local/share/piper-voices
# Download en_US-lessac-medium from https://github.com/rhasspy/piper/releases
```

## Architecture Overview

```
┌───────────────────────────────────────────────────────────────────────────┐
│                        dictate-agent (Rust binary)                         │
├───────────────────────────────────────────────────────────────────────────┤
│                                                                           │
│  ┌──────────┐    ┌──────────┐    ┌──────────┐                            │
│  │ Capture  │───▶│  Whisper │───▶│  Router  │                            │
│  │  (cpal)  │    │ (candle) │    │ (Ollama) │                            │
│  └──────────┘    └──────────┘    └────┬─────┘                            │
│       │                               │                                   │
│       │              ┌────────────────┼────────────────┐                  │
│       │              │                │                │                  │
│       │              ▼                ▼                ▼                  │
│       │       ┌──────────┐    ┌──────────┐    ┌──────────┐               │
│       │       │  Claude  │    │   Text   │    │ Command  │               │
│       │       │   Code   │    │  Editor  │    │  (keys)  │               │
│       │       │(process) │    │ (Haiku)  │    │          │               │
│       │       └────┬─────┘    └────┬─────┘    └────┬─────┘               │
│       │            │               │               │                      │
│       │            ▼               ▼               ▼                      │
│       │       ┌──────────┐   ┌──────────┐   ┌──────────┐                 │
│       │       │ Response │   │ Clipboard│   │  enigo   │                 │
│       │       │ Stream   │   │ Replace  │   │   key    │                 │
│       │       └────┬─────┘   └──────────┘   └──────────┘                 │
│       │            │                              │                       │
│       ▼            ▼                              ▼                       │
│  ┌──────────┐ ┌──────────┐                 ┌──────────┐                  │
│  │notify-   │ │  enigo   │                 │  Shell   │                  │
│  │rust      │ │ (type)   │                 │ Command  │                  │
│  └──────────┘ └────┬─────┘                 └──────────┘                  │
│                    │                                                      │
│              ┌─────┴─────┐                                                │
│              │  piper-rs │                                                │
│              │   (TTS)   │                                                │
│              └───────────┘                                                │
└───────────────────────────────────────────────────────────────────────────┘

Signal Flow:
  $mod+m (i3) ──SIGUSR1──▶ dictate-agent daemon

Mode Routing (via Ollama qwen3:0.6b):
  "edit: fix grammar"     ──▶ EDIT     ──▶ Haiku transform ──▶ Replace text
  "close window"          ──▶ COMMAND  ──▶ enigo key super+q
  "volume up"             ──▶ COMMAND  ──▶ pactl shell command
  "explain this function" ──▶ SONNET   ──▶ Claude Code ──▶ Stream + TTS
```

### Data Flow

1. **User presses `$mod+m`** → sends SIGUSR1 to daemon
2. **Daemon starts recording** → shows "Listening..." toast (notify-rust)
3. **User presses `$mod+m` again** → stops recording
4. **candle-whisper transcribes** → shows "Transcribing..." toast
5. **Ollama (qwen3:0.6b) classifies** → determines mode + model
6. **Route to handler**:
   - EDIT → capture text, send to Haiku, replace
   - COMMAND → execute via enigo or shell
   - HAIKU/SONNET/OPUS → Claude Code execution
7. **Response streams** → toast turns green, enigo types
8. **If general question** → piper-rs speaks response in parallel

### Module Structure

```rust
// src/main.rs
mod agent;      // Main daemon, signal handling, orchestration
mod capture;    // Audio recording via cpal
mod transcribe; // Whisper via candle
mod router;     // Ollama classification + fallback keywords
mod executor;   // Claude Code subprocess, stream parsing
mod output;     // enigo typing, piper-rs TTS
mod notify;     // notify-rust notification management
mod config;     // TOML configuration loading
mod editor;     // Text field capture/replace
mod commander;  // Command execution (keyboard + shell)
mod commands;   // Static command maps
```

## Project Structure

```
dictate_agent/
├── src/
│   ├── main.rs           # Entry point, tokio runtime
│   ├── agent.rs          # Main daemon and signal handling
│   ├── capture.rs        # Audio recording via cpal
│   ├── transcribe.rs     # Whisper via candle-whisper
│   ├── router.rs         # Ollama classification + fallback
│   ├── executor.rs       # Claude Code subprocess management
│   ├── output.rs         # enigo typing + piper-rs TTS
│   ├── notify.rs         # notify-rust notifications
│   ├── config.rs         # TOML configuration
│   ├── editor.rs         # Text field manipulation
│   ├── commander.rs      # Keyboard/shell command execution
│   └── commands.rs       # Static command maps
├── scripts/
│   └── dictate-toggle    # Shell script for i3 keybind
├── config/
│   ├── config.example.toml
│   └── commands.toml     # Custom command mappings
├── models/
│   └── .gitkeep          # Whisper model cache location
├── systemd/
│   └── dictate-agent.service
├── thoughts/
│   └── shared/
│       └── project/
│           └── 2026-01-15-dictate-agent.md
├── Cargo.toml
├── Cargo.lock
├── build.rs              # CUDA build configuration
└── README.md
```

## Configuration

```toml
# ~/.config/dictate-agent/config.toml

[whisper]
model = "base.en"
device = "cuda"           # or "cpu"

[router]
# Ollama configuration
ollama_host = "http://localhost:11434"
ollama_model = "qwen3:0.6b"
ollama_timeout_ms = 500

# Fallback keywords (if Ollama unavailable)
haiku_keywords = ["easy", "quick", "simple", "haiku"]
sonnet_keywords = ["medium", "normal", "sonnet"]
opus_keywords = ["hard", "complex", "difficult", "opus", "analyze"]

# Word count thresholds (fallback only)
short_threshold = 20
long_threshold = 100

# Code-related terms that bump complexity
code_terms = ["code", "function", "bug", "error", "refactor", "implement", "debug"]

default_model = "sonnet"

[tts]
enabled = true
voice_model = "en_US-lessac-medium"
voice_path = "~/.local/share/piper-voices"
# Keywords that trigger speech output
speak_triggers = ["what is", "who is", "how do", "why", "explain", "tell me"]

[output]
typing_delay_ms = 10
use_clipboard = false

[notifications]
enabled = true
timeout_ms = 30000

[editor]
enabled = true
triggers = ["edit:", "fix:", "change:", "rewrite:", "transform:"]
model = "haiku"

[commands]
enabled = true
custom_commands = "~/.config/dictate-agent/commands.toml"
fuzzy_threshold = 0.8    # Similarity threshold for fuzzy matching
confirm_destructive = true
destructive_patterns = ["kill", "close", "exit", "shutdown", "restart i3"]
```

## Development Workflow

### Setup
```bash
cd ~/dictate_agent

# Install Rust (if needed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install system dependencies
sudo pacman -S alsa-lib cuda
yay -S ollama

# Ensure Ollama is running with router model
ollama serve &
ollama pull qwen3:0.6b

# Build (release for performance)
cargo build --release

# Copy config
mkdir -p ~/.config/dictate-agent
cp config/config.example.toml ~/.config/dictate-agent/config.toml
```

### Running Locally
```bash
# Start daemon (release build)
./target/release/dictate-agent

# Or in dev mode with logging
RUST_LOG=debug cargo run
```

### Testing
```bash
# Run tests
cargo test

# Test individual components
cargo run --example test_router -- "what is the capital of France"
cargo run --example test_whisper -- test.wav
```

### i3 Integration
```bash
# Add to ~/.config/i3/config
exec --no-startup-id ~/dictate_agent/target/release/dictate-agent
bindsym $mod+m exec --no-startup-id kill -USR1 $(cat ~/.config/dictate-agent/dictate.pid)
```

## Router Decision Matrix

### Ollama Classification (Primary)

The router sends the transcription to qwen3:0.6b with a classification prompt. The model returns one of:
- `HAIKU` → Simple questions, quick lookups
- `SONNET` → Normal tasks, typical complexity
- `OPUS` → Complex analysis, architecture decisions
- `EDIT` → Text transformation requests
- `COMMAND` → System/keyboard commands

### Keyword Fallback (If Ollama Unavailable)

| Signal | Category |
|--------|----------|
| Starts with "edit:", "fix:", "change:" | EDIT |
| Matches static command map | COMMAND |
| Contains "easy", "quick", "simple" | HAIKU |
| Contains "hard", "complex", "analyze" | OPUS |
| Word count < 20, no code terms | HAIKU |
| Word count > 100 | OPUS |
| Default | SONNET |

## Success Criteria

### Technical Success
- [ ] Single binary under 50MB (release build)
- [ ] Startup time < 500ms (warm cache)
- [ ] End-to-end latency < 3 seconds for Haiku requests
- [ ] Ollama classification < 500ms
- [ ] Streaming response starts within 2 seconds of Claude receiving prompt
- [ ] TTS playback starts within 500ms of response beginning
- [ ] No dropped audio during recording

### User Success
- [ ] Can ask quick questions without waiting for Opus
- [ ] Complex requests automatically get appropriate compute
- [ ] Visual feedback always indicates system state
- [ ] General questions are spoken aloud naturally
- [ ] Integrates seamlessly with existing i3 workflow
- [ ] Works without network for commands/edit (Ollama local)

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| candle-whisper less accurate than faster-whisper | L | M | Test extensively, keep Python fallback option |
| Ollama latency too high | L | L | Pre-warm model, keyword fallback |
| CUDA setup complexity | M | H | Document setup, provide CPU fallback |
| enigo X11 issues | L | M | Test extensively, xdotool subprocess fallback |
| Binary size too large | M | L | Use release build, strip symbols |
| Compilation time | M | L | Incremental builds, use cargo-watch |

## Open Questions

1. **Silence detection**: Should we auto-stop recording after N seconds of silence?
   - **Tentative**: Start with manual toggle, add silence detection as enhancement

2. **Interrupt mechanism**: How to cancel mid-response?
   - **Tentative**: Second `$mod+m` press cancels, or `$mod+Shift+m`

3. **Ollama cold start**: How to handle if Ollama not running?
   - **Tentative**: Auto-start Ollama, or fall back to keyword matching

4. **Model preloading**: Should we keep Whisper model in memory?
   - **Tentative**: Yes, load on daemon start for instant transcription

## Implementation Phases

### Phase 1: Core Pipeline (MVP)
- Signal-based daemon with tokio
- cpal audio recording
- candle-whisper transcription
- Basic keyword router (no Ollama yet)
- Claude Code subprocess with streaming
- enigo text typing
- notify-rust notifications

### Phase 2: Ollama Integration
- ollama-rs client setup
- Classification prompt engineering
- Fallback to keywords if Ollama unavailable
- Router decision logging

### Phase 3: TTS Integration
- piper-rs setup with ort
- General question detection
- Parallel speech + typing via tokio::spawn
- Voice model configuration

### Phase 4: Text Editing Mode
- arboard clipboard access
- Edit trigger detection
- Haiku integration for transformation
- Field replacement via enigo

### Phase 5: Keyboard Command Mode
- Static command maps in commands.rs
- Fuzzy matching with strsim
- enigo key execution
- Shell command execution

### Phase 6: Polish
- Systemd service file
- Performance optimization
- Error recovery
- Logging and diagnostics

## References

- [candle-whisper](https://github.com/huggingface/candle) - Pure Rust Whisper
- [piper-rs](https://github.com/thewh1teagle/piper-rs) - Rust Piper TTS
- [enigo](https://github.com/enigo-rs/enigo) - Rust X11 automation
- [ollama-rs](https://github.com/pepperoni21/ollama-rs) - Ollama Rust client
- [Claude Code CLI Docs](https://code.claude.com/docs/en/headless) - Headless mode
- [cpal](https://github.com/RustAudio/cpal) - Rust audio I/O
- [notify-rust](https://github.com/hoodie/notify-rust) - Rust notifications

## Next Steps

1. [ ] Review and finalize this spec
2. [ ] Initialize Cargo project with dependencies
3. [ ] Implement Phase 1 core pipeline
4. [ ] Test Whisper accuracy vs soupawhisper
5. [ ] Add Ollama routing
