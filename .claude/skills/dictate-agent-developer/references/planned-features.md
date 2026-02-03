# Planned Features (from original spec)

These features are detected by routing but not implemented. The original spec (`thoughts/shared/project/2026-01-15-dictate-agent.md`) contains detailed behavioral specifications.

## EDIT Mode (Phase 4)

**Purpose**: Voice commands that transform text in the active input field.

**Trigger**: Prefix-based — "edit:", "fix:", "change:", "rewrite:", "transform:"

**Intended data flow**:
1. Router classifies as EDIT
2. Capture current field text via `Ctrl+A` → `Ctrl+C` (xdotool or enigo)
3. Read clipboard content
4. Send text + instruction to Haiku for fast transformation
5. Replace field content via `Ctrl+A` → type/paste new text
6. Show "Edited!" notification

**Example commands**:
- "edit: remove all instances of purple"
- "fix: correct the grammar"
- "change: make this more formal"
- "edit: replace foo with bar"

**Config already exists**: `EditorConfig` in `config.py:59-67` with `enabled`, `triggers`, `model` fields. Not yet wired to any executor.

**Implementation needs**:
- Clipboard access (Python: `subprocess.run(["xclip", "-selection", "clipboard", "-o"])` or `xdotool` approach)
- New executor that sends original text + instruction to Claude Haiku
- xdotool `Ctrl+A`, `Ctrl+C`, `Ctrl+A` key sequences
- Integration in `_handle_route()` EDIT case (currently prints "not yet implemented")

## COMMAND Mode (Phase 5)

**Purpose**: Voice-triggered keyboard shortcuts and system commands.

**Trigger**: Router detects command-like phrases.

**Two categories**:

### Keyboard Commands
Static mapping of voice phrases to key combos:

| Category | Example Voice | Action |
|----------|--------------|--------|
| Window mgmt | "close window" | Super+Q |
| Workspaces | "workspace 1" | Super+A |
| Navigation | "focus left" | Super+H |
| Common | "save" | Ctrl+S |
| Screenshot | "screenshot" | Super+Shift+0 |

### Shell Commands
Voice phrases mapped to shell execution:

| Example Voice | Command |
|--------------|---------|
| "play"/"pause" | `playerctl play-pause` |
| "volume up" | `pactl set-sink-volume @DEFAULT_SINK@ +5%` |
| "mute" | `pactl set-sink-mute @DEFAULT_SINK@ toggle` |

**Config already exists**: `CommandConfig` in `config.py:70-79` with `enabled`, `fuzzy_threshold`, `confirm_destructive`, `destructive_patterns`. Not yet wired.

**Implementation needs**:
- Static command maps (dict of voice phrase → key combo or shell command)
- Fuzzy matching library (original spec used `strsim` in Rust; Python equivalent: `thefuzz` or `difflib.SequenceMatcher`)
- xdotool key sending for keyboard commands
- `subprocess.run` for shell commands
- Destructive command confirmation (notification asking "are you sure?")
- Custom commands from `~/.config/dictate-agent/commands.toml`

## TTS — Text-to-Speech (Phase 3)

**Purpose**: Speak responses aloud for general knowledge questions.

**Detection**: Keywords like "what is", "who is", "how do", "why", "explain", "tell me" indicate a general question vs. a coding task.

**Original plan**: Use piper-rs (Rust) / Piper TTS with ONNX runtime. For Python, alternatives include:
- `piper-tts` Python package
- `espeak-ng` (system tool, low quality)
- `edge-tts` (Microsoft Edge TTS, requires network)
- Custom Piper integration via subprocess

**Implementation needs**:
- General question detector (keyword-based or LLM classification)
- TTS engine integration
- Parallel output: type text AND speak simultaneously
- Config for voice model, enable/disable, speak triggers
- "don't speak" keyword to suppress TTS for a single request

**Original config spec**:
```toml
[tts]
enabled = true
voice_model = "en_US-lessac-medium"
voice_path = "~/.local/share/piper-voices"
speak_triggers = ["what is", "who is", "how do", "why", "explain", "tell me"]
```

## Known Technical Debts

From research and handoff documents:

1. **Audio cutoff**: parecord loses 50-100ms at end of recording. `pw-record` identified as fix but not implemented. See `thoughts/shared/handoffs/general/2026-01-21_13-35-03_audio-recording-cutoff-debug.md`.

2. **Speculative decoding disabled**: Incompatible with chunked pipeline for >30s audio. If re-enabling, must use sequential long-form approach. See `thoughts/shared/research/2026-01-30-whisper-long-form-transcription.md`.

3. **`no_speech_threshold` config unused**: Exists in WhisperConfig but neither old nor new transcription code references it.

4. **Systemd service outdated**: `systemd/dictate-agent.service` points to Rust binary path and uses `RUST_LOG`. Should point to `scripts/run.sh` or Python module.
