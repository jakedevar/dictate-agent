# Phase 1 Research: Core Pipeline (MVP)

## Key Findings

### candle-whisper
- Use `candle-core`, `candle-nn`, `candle-transformers` with `cuda` feature
- Load model via `hf-hub` from `openai/whisper-base.en`
- Audio must be 16kHz mono f32 samples
- Convert PCM to mel spectrogram via `audio::pcm_to_mel()`
- Use `m::multilingual::Decoder` for transcription
- Model files: `config.json`, `tokenizer.json`, `model.safetensors`
- Mel filters binary file needed from candle repo

### cpal audio recording
- Use `cpal::default_host()` and `default_input_device()`
- Build input stream with `build_input_stream()`
- Use `Arc<Mutex<Vec<f32>>>` for thread-safe buffer
- Use `try_lock()` in callback to avoid blocking audio thread
- Most hardware uses 48kHz - may need resampling to 16kHz
- `stream.play()` / `stream.pause()` for start/stop
- PipeWire compatible via ALSA backend

### enigo text typing
- Version 0.6.1 with x11rb backend (default)
- `Enigo::new(&Settings::default())`
- `enigo.text("...")` for fast string typing
- `enigo.key(Key::Unicode(ch), Click)` for single keys
- `Key::Return` for newline, `Key::Control` for modifiers
- `Direction::Press/Release` for modifier combos
- `linux_delay` setting controls inter-key delay
- No runtime dependencies (pure Rust x11rb)

### tokio signal handling
- `signal(SignalKind::user_defined1())` for SIGUSR1
- `signal.recv().await` in `tokio::select!` loop
- Use `Arc<AtomicBool>` for simple toggle state
- PID file: write `std::process::id()` to file
- Clean up PID file in Drop impl
- Handle SIGTERM/SIGINT for graceful shutdown

### notify-rust notifications
- `Notification::new().summary("...").body("...").show()`
- Works with dunst via D-Bus
- Can set timeout, icon, category
- Position controlled by dunst config

### Claude Code streaming
- Command: `claude -p "prompt" --model <model> --output-format stream-json`
- NDJSON format with `type` field
- Parse with `serde_json::from_str::<Value>()`
- Use `tokio::process::Command` with stdout piped
- Read lines with `BufReader::new(stdout).lines()`

## Patterns to Follow

### Module organization
```
src/
├── main.rs           # Entry point, runtime setup
├── agent.rs          # Main orchestration, signal handling
├── capture.rs        # Audio recording
├── transcribe.rs     # Whisper inference
├── router.rs         # Model selection (keywords only for Phase 1)
├── executor.rs       # Claude Code subprocess
├── output.rs         # enigo typing
├── notify.rs         # Notifications
└── config.rs         # TOML configuration
```

### State machine pattern
```rust
enum AgentState {
    Idle,
    Recording,
    Transcribing,
    Routing,
    Executing,
    Responding,
}
```

## Files to Create
- `src/main.rs` - Tokio runtime, module declarations
- `src/agent.rs` - Main daemon loop, state management
- `src/capture.rs` - cpal audio recording
- `src/transcribe.rs` - candle-whisper transcription
- `src/router.rs` - Keyword-based model routing
- `src/executor.rs` - Claude Code subprocess
- `src/output.rs` - enigo text typing
- `src/notify.rs` - Notification helpers
- `src/config.rs` - Configuration loading
