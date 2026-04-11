# Dictate Agent Rust Rewrite — Implementation Plan

## Overview

Rewrite the dictate-agent voice dictation daemon from Python (2,412 LOC / 13 modules) to Rust. The Rust version replaces subprocess calls with native crates (cpal for audio, enigo for typing, notify-rust for notifications), swaps HuggingFace Transformers for whisper-rs (whisper.cpp bindings with CUDA), and produces a single static binary. The status window (Tkinter) is dropped — notifications replace it. The config schema is cleaned up to remove dead sections.

**Not rewriting the archive**: The existing `src_rust_archive/` used `candle` for Whisper (CPU-only, CUDA broken). This plan starts fresh with `whisper-rs` and a different architecture.

## Current State Analysis

The Python daemon sits in `signal.pause()`, runs the entire pipeline synchronously inside signal handlers, and depends on 8 external programs via subprocess. The only heavyweight Python dependencies are `torch` (~2GB) and `transformers` for Whisper inference.

### Key Discoveries:
- `router.py:43-79` — routing is purely keyword-based (80 LOC of string matching), no Ollama classification
- `notify.py:37-39` — notifications are just `print()` calls, not actual desktop notifications
- `transcribe.py:103` — speculative decoding is configured but intentionally skipped
- `output.py:53-60` — uses clipboard paste (xclip + xdotool ctrl+v), not keystroke simulation
- `audio.py:35-42` — parecord captures 16kHz mono s16le WAV with 200ms latency buffer
- `grammar.py:73` — `think=False` disables Qwen3 chain-of-thought reasoning
- `status_window.py` — entire Tkinter overlay module (222 LOC) is eliminated in the rewrite
- `systemd/dictate-agent.service:9` — already points to `target/release/dictate-agent`

## Desired End State

A single Rust binary at `target/release/dictate-agent` that:
1. Runs as a signal-driven daemon (SIGUSR1 toggle, SIGUSR2 cancel, SIGINT/SIGTERM shutdown)
2. Captures audio via cpal (no parecord subprocess)
3. Transcribes via whisper-rs with CUDA (GGUF model, ~400-600ms latency)
4. Corrects grammar via Ollama (fail-open, qwen3:0.6b)
5. Routes by keyword (TYPE/LOCAL/TIMER) and dispatches accordingly
6. Types output via clipboard paste (arboard + enigo, no xclip/xdotool subprocesses)
7. Shows desktop notifications via notify-rust (no notify-send subprocess)
8. Logs interactions to SQLite via rusqlite
9. Manages media playback via playerctl subprocess (kept — no good native crate)
10. Creates timers via systemd-run subprocess (kept — most reliable approach)

### Verification:
- `cargo build --release` produces a single binary
- `./target/release/dictate-agent --check` reports dependency status
- Full pipeline works: signal → record → transcribe → grammar → route → type
- `scripts/dictate-toggle` and `scripts/dictate-cancel` work unchanged
- `systemctl --user start dictate-agent` works with updated service file

## What We're NOT Doing

- **No Wayland support** — X11-only, matching current behavior
- **No status window** — dropped entirely, notifications replace it
- **No Claude Code CLI integration** — Ollama-only, matching current actual behavior
- **No EDIT or COMMAND route implementation** — stubs only, same as Python
- **No speculative decoding** — whisper-rs doesn't support it; unnecessary at current latencies
- **No streaming Ollama responses** — blocking generate calls, same as Python
- **No TTS** — was never implemented, stays out of scope
- **Not continuing src_rust_archive/** — starting fresh with whisper-rs instead of candle

## Config Schema Cleanup

The Python config has 9 TOML sections. The Rust version cleans this to 7:

**Removed:**
- `[editor]` — EDIT route not implemented
- `[commands]` — COMMAND route not implemented
- `[status_window]` — eliminated from rewrite

**Renamed/restructured:**
- `[router]` removed — routing is hardcoded keywords, no config needed
- `[whisper]` simplified — GGUF model path replaces HF model ID; HF-specific fields removed
- `[output]` simplified — only `auto_type` remains; `typing_delay_ms` was unused, `use_clipboard` is always true now
- `[local]` added — extracted from implicit config spread across modules; replaces the old router.ollama_* fields for local executor

**New config.toml:**

```toml
[whisper]
model_path = "~/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin"
device = "cuda"                # "cuda" or "cpu"
no_speech_threshold = 0.6

[grammar]
enabled = true
host = "http://localhost:11434"
model = "qwen3:0.6b"
timeout_s = 10.0
min_words = 3

[local]
host = "http://localhost:11434"
model = "qwen3:14b"
timeout_s = 120.0

[output]
auto_type = true

[notifications]
enabled = true
timeout_ms = 3000

[history]
enabled = true
db_path = ""                   # empty = ~/.local/share/dictate-agent/history.db
max_response_length = 10000

[timer]
sound_enabled = true
sound_file = "~/.config/dictate-agent/sounds/timer_alarm.wav"
```

## Implementation Approach

Seven phases, each producing a testable artifact. Phases 1-4 build the core pipeline (signal → record → transcribe). Phase 5 adds output. Phase 6 adds Ollama. Phase 7 adds the remaining features. The minimal working demo arrives after Phase 5.

Architecture shift from Python: the Python daemon uses `signal.pause()` with synchronous handlers. The Rust version uses `tokio::select!` on signal streams from `signal-hook-tokio`, with the pipeline steps as async functions. Whisper and cpal (synchronous FFI) run in `tokio::spawn_blocking`.

---

## Phase 1: Scaffolding + Config + Pure Logic

### Overview
Set up the Cargo project, implement config parsing with the cleaned-up schema, port the router and duration parser. All code in this phase is pure logic with no I/O — fully unit-testable.

### Changes Required:

#### 1. Project Structure
**File**: `Cargo.toml` (new, at repo root)

```toml
[package]
name = "dictate-agent"
version = "0.2.0"
edition = "2021"

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# Audio capture
cpal = { version = "0.17", features = ["pipewire"] }

# Whisper transcription
whisper-rs = { version = "0.16", features = ["cuda"] }

# Ollama client
ollama-rs = { version = "0.3", features = ["stream"] }

# Clipboard
arboard = "3"

# Keyboard/mouse simulation
enigo = { version = "0.6", features = ["x11rb"] }

# Desktop notifications
notify-rust = "4"

# Signal handling
signal-hook = "0.4"
signal-hook-tokio = { version = "0.3", features = ["futures-v0_3"] }

# Config
toml = "1"
serde = { version = "1", features = ["derive"] }

# SQLite
rusqlite = { version = "0.31", features = ["bundled"] }

# JSON (for Ollama response parsing if needed)
serde_json = "1"

# Duration parsing
regex = "1"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Error handling
anyhow = "1"
thiserror = "2"

# Misc
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }

[profile.release]
opt-level = 3
lto = true
strip = true
```

**Directory layout** (under repo root):
```
src/
    main.rs           # Entry point, arg parsing, tokio runtime
    agent.rs          # DictateAgent struct, pipeline orchestration
    config.rs         # Serde-driven config from TOML
    router.rs         # Keyword routing (TYPE/LOCAL/TIMER/EDIT)
    audio.rs          # cpal PCM capture
    transcribe.rs     # whisper-rs inference
    output.rs         # arboard + enigo clipboard paste
    notify.rs         # notify-rust desktop notifications
    grammar.rs        # Ollama grammar correction
    local_executor.rs # Ollama local inference
    timer.rs          # Duration parser + systemd-run
    history.rs        # rusqlite interaction logging
```

#### 2. Config Module
**File**: `src/config.rs`

```rust
use serde::Deserialize;
use std::path::{Path, PathBuf};

// XDG paths
const CONFIG_DIR: &str = "dictate-agent";
const CONFIG_FILE: &str = "config.toml";
const PID_FILE: &str = "dictate.pid";
const MEDIA_STATE_FILE: &str = "media_was_playing";

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub whisper: WhisperConfig,
    pub grammar: GrammarConfig,
    pub local: LocalConfig,
    pub output: OutputConfig,
    pub notifications: NotificationConfig,
    pub history: HistoryConfig,
    pub timer: TimerConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct WhisperConfig {
    pub model_path: String,  // e.g. ~/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin
    pub device: String,      // "cuda" or "cpu"
    pub no_speech_threshold: f32,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct GrammarConfig {
    pub enabled: bool,
    pub host: String,
    pub model: String,
    pub timeout_s: f64,
    pub min_words: usize,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    pub host: String,
    pub model: String,
    pub timeout_s: f64,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub auto_type: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub timeout_ms: u32,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub db_path: String,
    pub max_response_length: usize,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct TimerConfig {
    pub sound_enabled: bool,
    pub sound_file: String,
}

// Default impls match the config.toml defaults shown above.
// load_config() reads the TOML file, deserializes with serde,
// and expands ~ in paths via shellexpand or manual replacement.

pub fn load_config(path: Option<&Path>) -> anyhow::Result<Config> {
    // Resolve path: argument > XDG_CONFIG_HOME/dictate-agent/config.toml > defaults
    // If file doesn't exist, return Config::default() (all defaults)
    // serde(default) on every struct means missing sections/fields are fine
    todo!()
}

pub fn config_dir() -> PathBuf {
    // XDG_CONFIG_HOME or ~/.config, then /dictate-agent
    todo!()
}

pub fn pid_file_path() -> PathBuf {
    config_dir().join(PID_FILE)
}

pub fn media_state_path() -> PathBuf {
    config_dir().join(MEDIA_STATE_FILE)
}
```

Key differences from Python `config.py`:
- `serde(default)` replaces the manual `.get(key, default)` pattern — unknown keys are ignored, missing keys use defaults
- No `[router]`, `[editor]`, `[commands]`, `[status_window]` sections
- `[local]` replaces the old router.ollama_* fields
- `[timer]` extracts settings that were hardcoded in Python's `timer_executor.py`
- Tilde expansion needed for `model_path`, `db_path`, `sound_file`

#### 3. Router Module
**File**: `src/router.rs`

Direct port of `dictate/router.py:43-79`. Pure function, no I/O.

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum RouteType {
    Type,
    Local,
    Timer,
    Edit,
    Command,
}

#[derive(Debug, Clone)]
pub struct RouteResult {
    pub route: RouteType,
    pub model: String,
    pub text: String,
    pub confidence: f64,
}

const EDIT_TRIGGERS: &[&str] = &["edit:", "fix:", "change:", "rewrite:", "transform:"];
const LOCAL_TRIGGERS: &[&str] = &["simple", "easy", "medium", "hard"];

pub fn route(text: &str) -> RouteResult {
    let text = text.trim();
    if text.is_empty() {
        return RouteResult { route: RouteType::Type, model: String::new(), text: text.into(), confidence: 1.0 };
    }

    // Check edit triggers (colon-suffixed prefixes)
    let lower = text.to_lowercase();
    for trigger in EDIT_TRIGGERS {
        if lower.starts_with(trigger) {
            return RouteResult {
                route: RouteType::Edit,
                model: String::new(),
                text: text[trigger.len()..].trim().into(),
                confidence: 1.0,
            };
        }
    }

    // Split on first whitespace
    let (first, rest) = match text.split_once(char::is_whitespace) {
        Some((f, r)) => (f, r.trim()),
        None => (text, ""),
    };
    let first_clean = first.to_lowercase();
    let first_clean = first_clean.trim_end_matches(&['.', ',', '!', '?', ':', ';'][..]);

    if first_clean == "timer" {
        return RouteResult { route: RouteType::Timer, model: String::new(), text: rest.into(), confidence: 1.0 };
    }
    if LOCAL_TRIGGERS.contains(&first_clean) {
        return RouteResult { route: RouteType::Local, model: "local".into(), text: rest.into(), confidence: 1.0 };
    }

    RouteResult { route: RouteType::Type, model: String::new(), text: text.into(), confidence: 1.0 }
}
```

#### 4. Duration Parser
**File**: `src/timer.rs` (parser portion only; systemd-run dispatch is Phase 7)

Port of `dictate/timer_executor.py:15-138`. The Python uses a dynamically constructed regex with word-to-number and unit-alias maps. The Rust port uses the `regex` crate with the same approach.

```rust
use std::collections::HashMap;
use regex::Regex;

/// Returns (total_seconds, remaining_text). None seconds = parse failure.
pub fn parse_duration(text: &str) -> (Option<u64>, String) {
    // Port of timer_executor.py:49-138
    // 1. Check "half hour" / "half an hour" prefix
    // 2. Build regex from WORD_TO_NUM keys + UNIT_ALIASES keys (longest-first)
    // 3. Greedy match loop at current position
    // 4. Fallback: search anywhere in original string
    // 5. Return (None, original) if nothing found
    todo!()
}

/// Formats seconds as systemd OnActiveSec duration (e.g., "1h30m")
pub fn format_systemd_duration(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    let mut result = String::new();
    if h > 0 { result.push_str(&format!("{}h", h)); }
    if m > 0 { result.push_str(&format!("{}m", m)); }
    if s > 0 || result.is_empty() { result.push_str(&format!("{}s", s)); }
    result
}

/// Formats seconds as human-readable string (e.g., "1 hour 30 minutes")
pub fn format_human_duration(seconds: u64) -> String {
    todo!()
}
```

Word-to-number map (port from Python `WORD_TO_NUM` at `timer_executor.py:15-22`):
```rust
lazy_static! or std::sync::LazyLock
    "one" => 1, "two" => 2, ..., "twenty" => 20, "thirty" => 30,
    "forty" => 40, "fifty" => 50, "sixty" => 60,
    "a" => 1, "an" => 1
```

Unit aliases map (port from `timer_executor.py:24-29`):
```rust
    "s" | "sec" | "secs" | "second" | "seconds" => Seconds,
    "m" | "min" | "mins" | "minute" | "minutes" => Minutes,
    "h" | "hr" | "hrs" | "hour" | "hours" => Hours,
```

### Success Criteria:

#### Automated Verification:
- [x] `cargo build` compiles without errors
- [x] `cargo test` passes unit tests for:
  - Config: default values, TOML parsing, missing sections, tilde expansion
  - Router: empty input, edit triggers, timer trigger, local triggers, default fallthrough, punctuation stripping
  - Duration parser: "5 minutes", "1 hour 30 minutes", "half an hour", "a minute", word numbers ("five minutes"), fallback search ("set a timer for 5 minutes"), invalid input returns None
  - Systemd duration formatting: 90 → "1m30s", 3600 → "1h", 0 → "0s"
- [x] `cargo clippy` passes with no warnings

#### Manual Verification:
- [ ] Config loads the example TOML file correctly
- [ ] All default values match the documented schema above

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 2: Daemon Core

### Overview
Implement the signal-driven main loop using tokio and signal-hook-tokio. The daemon writes a PID file, listens for SIGUSR1/SIGUSR2/SIGINT/SIGTERM, and shuts down cleanly. No pipeline logic yet — signal handlers just log the signal received.

### Changes Required:

#### 1. Entry Point
**File**: `src/main.rs`

```rust
use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod agent;
mod config;
mod router;
mod timer;
// ... other mod declarations added in later phases

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing (replaces Python's print statements)
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env()
            .add_directive("dictate_agent=info".parse()?))
        .init();

    // Parse args: --check flag for dependency verification
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--check") {
        return check_all_dependencies();
    }

    // Load config
    let config = config::load_config(None)?;

    // Create and run agent
    let mut agent = agent::DictateAgent::new(config).await?;
    agent.run().await
}

fn check_all_dependencies() -> Result<()> {
    // Check each external program: playerctl, systemd-run, dunstify, play (sox), ollama
    // Print status table, exit 0 if all found, exit 1 if any missing
    todo!()
}
```

#### 2. Agent State Machine
**File**: `src/agent.rs`

```rust
use crate::config::Config;
use anyhow::Result;
use signal_hook::consts::signal::{SIGINT, SIGTERM, SIGUSR1, SIGUSR2};
use signal_hook_tokio::Signals;
use tokio_stream::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn, error};

pub struct DictateAgent {
    config: Config,
    recording: bool,
    // Components added in later phases:
    // audio, transcriber, grammar, router uses the free function,
    // local_executor, timer_executor, output, notifier, history
}

impl DictateAgent {
    pub async fn new(config: Config) -> Result<Self> {
        // Write PID file
        let pid_path = crate::config::pid_file_path();
        if let Some(parent) = pid_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&pid_path, std::process::id().to_string())?;
        info!("PID file written to {}", pid_path.display());

        Ok(Self {
            config,
            recording: false,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut signals = Signals::new(&[SIGUSR1, SIGUSR2, SIGINT, SIGTERM])?;

        info!("Dictate agent running. Waiting for signals...");

        while let Some(signal) = signals.next().await {
            match signal {
                SIGUSR1 => self.toggle().await,
                SIGUSR2 => self.cancel().await,
                SIGINT | SIGTERM => {
                    info!("Shutdown signal received");
                    break;
                }
                _ => unreachable!(),
            }
        }

        self.shutdown().await;
        Ok(())
    }

    async fn toggle(&mut self) {
        if self.recording {
            info!("Stopping recording, processing pipeline...");
            self.recording = false;
            // Phase 3+: self.stop_recording_and_process().await
        } else {
            info!("Starting recording...");
            self.recording = true;
            // Phase 3+: self.start_recording().await
        }
    }

    async fn cancel(&mut self) {
        if self.recording {
            info!("Recording cancelled");
            self.recording = false;
            // Phase 3+: self.audio.stop(); self.audio.cleanup();
        } else {
            warn!("Cancel received but not recording");
        }
    }

    async fn shutdown(&mut self) {
        info!("Shutting down...");
        // Delete PID file
        let pid_path = crate::config::pid_file_path();
        let _ = std::fs::remove_file(&pid_path);
        // Phase 7: close history DB, cleanup temp files
    }
}

// Drop impl to ensure PID file cleanup on panic
impl Drop for DictateAgent {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(crate::config::pid_file_path());
    }
}
```

Key architecture difference from Python: the Python uses `signal.pause()` and runs the pipeline synchronously inside the signal handler. The Rust version uses `tokio::select!`-style signal stream (via `signal_hook_tokio::Signals` which implements `Stream`), and the pipeline runs as async functions. This means a second signal arriving during pipeline execution is buffered until the current `toggle().await` completes — same serialization guarantee as the Python GIL-locked signal handler approach.

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build --release` produces a binary at `target/release/dictate-agent`
- [ ] Binary size is under 50MB (release, stripped, LTO)
- [ ] `cargo test` passes (existing Phase 1 tests still pass)
- [ ] `cargo clippy` clean

#### Manual Verification:
- [ ] `./target/release/dictate-agent` starts and prints "Waiting for signals..."
- [ ] PID file appears at `~/.config/dictate-agent/dictate.pid`
- [ ] `scripts/dictate-toggle` (kill -USR1) causes "Starting recording..." log
- [ ] Second `scripts/dictate-toggle` causes "Stopping recording..." log
- [ ] `scripts/dictate-cancel` causes "Recording cancelled" log
- [ ] Ctrl+C causes "Shutdown signal received" and PID file is deleted
- [ ] `./target/release/dictate-agent --check` prints dependency status

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: Audio Capture

### Overview
Replace `parecord` subprocess with native audio capture via cpal. Capture 16kHz mono PCM directly into a memory buffer. No WAV file needed — whisper-rs accepts raw f32 samples.

### Changes Required:

#### 1. Audio Module
**File**: `src/audio.rs`

```rust
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, SampleFormat, StreamConfig};
use std::sync::{Arc, Mutex};
use anyhow::Result;
use tracing::{info, warn, error};

pub struct AudioCapture {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Vec<f32>>>,
    is_recording: bool,
}

impl AudioCapture {
    pub fn new() -> Result<Self> {
        Ok(Self {
            stream: None,
            buffer: Arc::new(Mutex::new(Vec::new())),
            is_recording: false,
        })
    }

    pub fn start(&mut self) -> Result<()> {
        if self.is_recording {
            anyhow::bail!("Already recording");
        }

        let host = cpal::default_host();
        let device = host.default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No input device found"))?;

        // Request 16kHz mono — PipeWire/PulseAudio will resample if needed
        let config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(16000),
            buffer_size: cpal::BufferSize::Default,
        };

        let buffer = self.buffer.clone();
        buffer.lock().unwrap().clear();

        // Build input stream — cpal delivers samples in the requested format
        // Handle both f32 and i16 sample formats from the device
        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                buffer.lock().unwrap().extend_from_slice(data);
            },
            |err| error!("Audio stream error: {}", err),
            None, // no timeout
        )?;

        stream.play()?;
        self.stream = Some(stream);
        self.is_recording = true;
        info!("Recording started (16kHz mono f32)");
        Ok(())
    }

    /// Stops recording and returns the audio buffer as f32 samples at 16kHz.
    /// Adds a trailing 500ms capture delay (matching Python behavior).
    pub async fn stop(&mut self) -> Option<Vec<f32>> {
        if !self.is_recording {
            return None;
        }

        // Trailing audio capture delay (matches audio.py:60 — time.sleep(0.5))
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Drop the stream to stop recording
        self.stream.take();
        self.is_recording = false;

        let samples = std::mem::take(&mut *self.buffer.lock().unwrap());
        let duration = samples.len() as f64 / 16000.0;
        info!("Recording stopped: {:.1}s ({} samples)", duration, samples.len());

        if samples.is_empty() {
            None
        } else {
            Some(samples)
        }
    }

    pub fn is_recording(&self) -> bool {
        self.is_recording
    }

    /// Audio duration in seconds from a sample buffer
    pub fn duration_secs(samples: &[f32]) -> f64 {
        samples.len() as f64 / 16000.0
    }
}
```

Key differences from Python `audio.py`:
- No subprocess, no temp file — samples stay in memory as `Vec<f32>`
- No PipeWire buffer flush needed — cpal clears its buffer on stream creation
- No WAV header — whisper-rs takes raw f32 samples directly
- Duration calculated from sample count, not file size (Python's `(file_size - 44) / 32000` hack)
- The 500ms trailing capture delay is preserved (matches `audio.py:60`)
- cpal's `build_input_stream` handles sample format conversion

**Note on sample format**: cpal may deliver i16 samples on some ALSA backends. If the default device doesn't support f32, add a fallback path that builds an i16 stream and converts `sample as f32 / 32768.0` in the callback. Check `device.supported_input_configs()` to pick the best format.

#### 2. Wire into Agent
**File**: `src/agent.rs` (additions)

Add `audio: AudioCapture` field to `DictateAgent`. In `toggle()`:
- If not recording: call `self.audio.start()`
- If recording: call `self.audio.stop().await` to get the sample buffer

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build` compiles with cpal PipeWire feature
- [ ] `cargo test` passes (unit test: `AudioCapture::duration_secs` correctness)
- [ ] `cargo clippy` clean

#### Manual Verification:
- [ ] Start daemon, send SIGUSR1, speak, send SIGUSR1 again
- [ ] Log shows "Recording started" and "Recording stopped: X.Xs (N samples)"
- [ ] Audio duration matches approximate speech length
- [ ] No ALSA/PipeWire errors in the log
- [ ] Second recording works (buffer properly cleared)

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 4: Whisper Transcription

### Overview
Integrate whisper-rs for GPU-accelerated speech-to-text. Load the GGUF model in a background task at startup. Transcription runs in `spawn_blocking` since whisper-rs is synchronous FFI.

### Prerequisites
Download the GGUF model before testing:
```bash
mkdir -p ~/.local/share/dictate-agent/models
wget -O ~/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
```

### Changes Required:

#### 1. Transcription Module
**File**: `src/transcribe.rs`

```rust
use whisper_rs::{WhisperContext, WhisperContextParameters, FullParams, SamplingStrategy};
use std::sync::Arc;
use tokio::sync::OnceCell;
use anyhow::Result;
use tracing::{info, warn, error};

#[derive(Debug, Clone)]
pub struct TranscriptionResult {
    pub text: String,
    pub language: String,
    pub duration_s: f64,
}

pub struct Transcriber {
    context: Arc<OnceCell<WhisperContext>>,
    model_path: String,
    use_gpu: bool,
    no_speech_threshold: f32,
}

impl Transcriber {
    pub fn new(config: &crate::config::WhisperConfig) -> Self {
        Self {
            context: Arc::new(OnceCell::new()),
            model_path: config.model_path.clone(), // tilde-expanded by config loader
            use_gpu: config.device == "cuda",
            no_speech_threshold: config.no_speech_threshold,
        }
    }

    /// Start loading the model in the background.
    /// Call this during DictateAgent::new() — first transcribe() will await completion.
    pub fn load_model_async(&self) {
        let ctx = self.context.clone();
        let path = self.model_path.clone();
        let use_gpu = self.use_gpu;

        tokio::spawn(async move {
            let start = std::time::Instant::now();
            info!("Loading Whisper model from {}...", path);

            let result = tokio::task::spawn_blocking(move || {
                let mut params = WhisperContextParameters::default();
                params.use_gpu(use_gpu);
                WhisperContext::new_with_params(&path, params)
            }).await;

            match result {
                Ok(Ok(context)) => {
                    let elapsed = start.elapsed();
                    info!("Whisper model loaded in {:.1}s", elapsed.as_secs_f64());
                    let _ = ctx.set(context);
                }
                Ok(Err(e)) => error!("Failed to load Whisper model: {}", e),
                Err(e) => error!("Model loading task panicked: {}", e),
            }
        });
    }

    /// Transcribe audio samples (f32, 16kHz mono).
    /// Blocks until model is loaded if still loading.
    pub async fn transcribe(&self, samples: &[f32]) -> Result<Option<TranscriptionResult>> {
        let ctx = self.context.get_or_init(|| async {
            // Fallback: if load_model_async wasn't called, load synchronously
            panic!("Model not loaded — call load_model_async() first");
        }).await;

        let audio_duration = samples.len() as f64 / 16000.0;
        let samples = samples.to_vec(); // Clone for spawn_blocking move
        let no_speech_thresh = self.no_speech_threshold;

        let start = std::time::Instant::now();

        // whisper-rs is synchronous FFI — run in blocking thread
        let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let mut state = ctx.create_state()?;

            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_language(Some("en"));
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_no_speech_thold(no_speech_thresh);
            params.set_n_max_text_ctx(128);  // matches Python max_new_tokens=128

            state.full(params, &samples)?;

            let n_segments = state.full_n_segments()?;
            if n_segments == 0 {
                return Ok(None);
            }

            let mut text = String::new();
            for i in 0..n_segments {
                text.push_str(state.full_get_segment_text(i)?);
            }

            let text = text.trim().to_string();
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(text))
            }
        }).await??;

        let transcription_time = start.elapsed().as_secs_f64();
        info!("Transcription: {:.3}s for {:.1}s audio", transcription_time, audio_duration);

        match result {
            Some(text) => {
                let corrected = apply_corrections(&text);
                Ok(Some(TranscriptionResult {
                    text: corrected,
                    language: "en".into(),
                    duration_s: audio_duration,
                }))
            }
            None => Ok(None),
        }
    }
}

/// Hardcoded Whisper mis-transcription corrections.
/// Port of transcribe.py:186-218.
fn apply_corrections(text: &str) -> String {
    let corrections = [
        ("Clod", "Claude"),
        ("Cloud", "Claude"),
        ("Clawed", "Claude"),
        ("clod", "Claude"),
        ("cloud", "Claude"),
        ("clawed", "Claude"),
        // Slash-command rewrites
        ("research codebase", "/research_codebase"),
        ("research code base", "/research_codebase"),
        // ... port all 14 pairs from transcribe.py:191-217
    ];

    let mut result = text.to_string();
    for (from, to) in corrections {
        result = result.replace(from, to);
    }
    result
}
```

Key differences from Python `transcribe.py`:
- No `threading.Event` synchronization — uses `tokio::sync::OnceCell` which is async-native
- No WAV file I/O — takes f32 samples directly from the audio buffer
- No HuggingFace pipeline — whisper-rs provides direct access to whisper.cpp
- No speculative decoding — whisper.cpp doesn't support it, and it was never active in Python anyway
- `spawn_blocking` for the FFI call ensures the tokio runtime isn't blocked during inference

**Note on OnceCell**: `tokio::sync::OnceCell` allows the first `transcribe()` call to await model loading completion, similar to Python's `model_loaded.wait()`. If the model is already loaded, it returns immediately.

#### 2. Wire into Agent
**File**: `src/agent.rs` (additions)

Add `transcriber: Transcriber` field. In `DictateAgent::new()`, call `transcriber.load_model_async()`. In the pipeline (after `audio.stop()`), call `transcriber.transcribe(&samples).await`.

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build --release` compiles with whisper-rs CUDA feature
- [ ] `cargo test` passes (unit test: `apply_corrections` table)
- [ ] `cargo clippy` clean

#### Manual Verification:
- [ ] Daemon starts, log shows "Loading Whisper model..." then "loaded in X.Xs"
- [ ] Model loads faster than Python (~3-5s vs ~15-30s)
- [ ] Record speech → log shows transcription text and timing
- [ ] Transcription quality matches Python version (same model, just different runtime)
- [ ] GPU utilization visible during transcription (nvidia-smi)
- [ ] Empty/silent recording produces no transcription (no_speech_threshold working)

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 5: Output + Notifications

### Overview
Replace xclip + xdotool subprocesses with arboard (clipboard) + enigo (key simulation). Replace the print-only notifier with notify-rust for real desktop notifications. After this phase, the minimal pipeline works end-to-end: signal → record → transcribe → type.

### Changes Required:

#### 1. Output Module
**File**: `src/output.rs`

```rust
use arboard::Clipboard;
use enigo::{Enigo, Settings, Key, Keyboard, Direction};
use anyhow::Result;
use tracing::{info, warn, error};

pub struct OutputHandler {
    auto_type: bool,
}

impl OutputHandler {
    pub fn new(config: &crate::config::OutputConfig) -> Self {
        Self {
            auto_type: config.auto_type,
        }
    }

    /// Type text by setting clipboard and simulating Ctrl+V.
    /// Saves and restores the previous clipboard content.
    /// Returns true on success.
    pub fn type_text(&self, text: &str) -> bool {
        if text.trim().is_empty() || !self.auto_type {
            return false;
        }

        let result = self.type_text_inner(text);
        match &result {
            Ok(()) => {
                info!("Typed {} characters", text.len());
                true
            }
            Err(e) => {
                error!("Failed to type text: {}", e);
                false
            }
        }
    }

    fn type_text_inner(&self, text: &str) -> Result<()> {
        let mut clipboard = Clipboard::new()?;

        // Save current clipboard (best effort)
        let saved = clipboard.get_text().ok();

        // Set clipboard to our text
        clipboard.set_text(text.trim())?;

        // Simulate Ctrl+V paste
        let mut enigo = Enigo::new(&Settings::default())?;
        enigo.key(Key::Control, Direction::Press)?;
        enigo.key(Key::Unicode('v'), Direction::Click)?;
        enigo.key(Key::Control, Direction::Release)?;

        // Wait for paste to land (matches output.py:63 — 50ms)
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Restore previous clipboard (best effort)
        if let Some(saved) = saved {
            let _ = clipboard.set_text(&saved);
        }

        Ok(())
    }
}
```

Key differences from Python `output.py`:
- arboard replaces 3 xclip subprocess calls (read, write, restore)
- enigo replaces xdotool subprocess for Ctrl+V simulation
- `--clearmodifiers` flag (xdotool) is handled by enigo internally
- The 50ms paste delay is preserved
- `typing_delay_ms` field is dropped (was never used in Python)

#### 2. Notification Module
**File**: `src/notify.rs`

```rust
use notify_rust::{Notification, Timeout};
use tracing::{info, error};

pub struct Notifier {
    enabled: bool,
    timeout_ms: u32,
    app_name: String,
}

impl Notifier {
    pub fn new(config: &crate::config::NotificationConfig) -> Self {
        Self {
            enabled: config.enabled,
            timeout_ms: config.timeout_ms,
            app_name: "Dictate Agent".into(),
        }
    }

    fn notify(&self, title: &str, message: &str, icon: &str, timeout_ms: Option<u32>) {
        if !self.enabled {
            return;
        }
        let timeout = timeout_ms.unwrap_or(self.timeout_ms);
        if let Err(e) = Notification::new()
            .appname(&self.app_name)
            .summary(title)
            .body(message)
            .icon(icon)
            .timeout(Timeout::Milliseconds(timeout))
            .show()
        {
            error!("Notification failed: {}", e);
        }
    }

    pub fn recording(&self) {
        self.notify("Recording...", "Speak now", "media-record", Some(30000));
    }

    pub fn transcribing(&self) {
        self.notify("Transcribing...", "Processing audio", "media-playback-start", Some(30000));
    }

    pub fn processing(&self, model: &str) {
        self.notify("Processing...", &format!("Using {}", model), "system-run", Some(30000));
    }

    pub fn done(&self, text: &str) {
        let display = if text.len() > 100 { &text[..100] } else { text };
        self.notify("Done", display, "dialog-ok", None);
    }

    pub fn error(&self, message: &str) {
        let display = if message.len() > 100 { &message[..100] } else { message };
        self.notify("Error", display, "dialog-error", Some(5000));
    }

    pub fn no_speech(&self) {
        self.notify("No Speech", "No speech detected in recording", "dialog-warning", Some(2000));
    }

    pub fn cancelled(&self) {
        self.notify("Cancelled", "Recording discarded", "dialog-cancel", Some(2000));
    }
}
```

This is the first time dictate-agent will have **real desktop notifications**. The Python `notify.py` was just `print()` calls. notify-rust uses D-Bus directly — no subprocess, no external tool dependency.

**Icon conventions** (from `.claude/skills/dictate-agent-developer/SKILL.md`):
- Recording: `media-record`
- Transcribing: `media-playback-start`
- Processing: `system-run`
- Done: `dialog-ok`
- Error: `dialog-error`
- Cancelled/no-speech: `dialog-warning`/`dialog-cancel`

#### 3. Wire into Agent Pipeline
**File**: `src/agent.rs` (additions)

Add `output: OutputHandler` and `notifier: Notifier` fields. Wire the minimal pipeline:

```rust
async fn stop_recording_and_process(&mut self) {
    self.notifier.transcribing();

    // 1. Stop recording
    let samples = match self.audio.stop().await {
        Some(s) => s,
        None => {
            warn!("No audio captured");
            return;
        }
    };

    // 2. Transcribe
    let result = match self.transcriber.transcribe(&samples).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            self.notifier.no_speech();
            return;
        }
        Err(e) => {
            error!("Transcription failed: {}", e);
            self.notifier.error(&format!("Transcription failed: {}", e));
            return;
        }
    };

    info!("Transcribed: \"{}\"", result.text);

    // 3. Route (phases 6-7 add grammar + executor dispatch)
    let route = crate::router::route(&result.text);

    // 4. For now, TYPE route only — just type the text
    match route.route {
        crate::router::RouteType::Type => {
            self.output.type_text(&route.text);
        }
        other => {
            warn!("Route {:?} not yet implemented", other);
        }
    }

    self.notifier.done(&result.text);
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build --release` compiles with arboard, enigo, notify-rust
- [ ] `cargo test` passes
- [ ] `cargo clippy` clean

#### Manual Verification:
- [ ] **End-to-end test**: Start daemon → SIGUSR1 → speak "hello world" → SIGUSR1 → text appears in focused text field
- [ ] Desktop notification appears for recording/transcribing/done states
- [ ] Clipboard content is preserved after typing (save/restore works)
- [ ] Ctrl+V paste works correctly in various applications (terminal, browser, text editor)
- [ ] "timer five minutes" logs "Route Timer not yet implemented" (correct routing, just not dispatched yet)
- [ ] "easy what is the weather" logs "Route Local not yet implemented"

**Implementation Note**: This is the first end-to-end milestone. After verifying the minimal pipeline works, pause for confirmation before adding Ollama integration.

---

## Phase 6: Ollama Integration

### Overview
Port grammar correction and local executor to use ollama-rs async client. Preserve the fail-open pattern for grammar and the never-raise contract for executors.

### Changes Required:

#### 1. Grammar Correction
**File**: `src/grammar.rs`

```rust
use anyhow::Result;
use tracing::{info, warn, error};
use std::time::{Duration, Instant};

const GRAMMAR_PROMPT: &str = r#"Fix only grammar, spelling, and punctuation errors in this text. Do not change the meaning, add words, remove words, or rephrase. Output only the corrected text, nothing else.

Text: {text}"#;

#[derive(Debug, Clone)]
pub struct GrammarResult {
    pub success: bool,
    pub corrected: String,
    pub original: String,
    pub duration_s: f64,
    pub error: Option<String>,
}

pub struct GrammarCorrector {
    enabled: bool,
    host: String,
    port: u16,
    model: String,
    timeout: Duration,
    min_words: usize,
}

impl GrammarCorrector {
    pub fn new(config: &crate::config::GrammarConfig) -> Self {
        // Parse host:port from config.host URL
        let (host, port) = parse_host_port(&config.host);
        Self {
            enabled: config.enabled,
            host,
            port,
            model: config.model.clone(),
            timeout: Duration::from_secs_f64(config.timeout_s),
            min_words: config.min_words,
        }
    }

    /// Correct grammar. NEVER returns an error — fail-open.
    /// On any failure, returns the original text in `corrected`.
    pub async fn correct(&self, text: &str) -> GrammarResult {
        let original = text.to_string();
        let start = Instant::now();

        // Fast paths: disabled or too short
        if !self.enabled {
            return GrammarResult::pass_through(original, start);
        }
        if text.split_whitespace().count() < self.min_words {
            return GrammarResult::pass_through(original, start);
        }

        // Call Ollama
        match self.call_ollama(text).await {
            Ok(corrected) => {
                let ratio = corrected.len() as f64 / text.len() as f64;
                if ratio < 0.5 || ratio > 1.5 {
                    warn!("Grammar correction rejected: length ratio {:.2}", ratio);
                    GrammarResult::fail(original, start, "Length ratio out of range")
                } else {
                    info!("Grammar corrected in {:.3}s", start.elapsed().as_secs_f64());
                    GrammarResult {
                        success: true,
                        corrected,
                        original,
                        duration_s: start.elapsed().as_secs_f64(),
                        error: None,
                    }
                }
            }
            Err(e) => {
                warn!("Grammar correction failed (fail-open): {}", e);
                GrammarResult::fail(original, start, &e.to_string())
            }
        }
    }

    async fn call_ollama(&self, text: &str) -> Result<String> {
        let ollama = ollama_rs::Ollama::new(&self.host, self.port);
        let prompt = GRAMMAR_PROMPT.replace("{text}", text);

        let request = ollama_rs::generation::completion::request::GenerationRequest::new(
            self.model.clone(),
            prompt,
        )
        .options(ollama_rs::generation::options::GenerationOptions::default()
            .num_predict(256)
            .temperature(0.1));
        // Note: ollama-rs may not have a direct `think` parameter equivalent.
        // If qwen3 outputs <think> tags, strip them from the response.

        let response = tokio::time::timeout(
            self.timeout,
            ollama.generate(request),
        ).await??;

        let text = response.response.trim().to_string();
        if text.is_empty() {
            anyhow::bail!("Empty response from grammar model");
        }

        // Strip any <think>...</think> wrapper if present
        let text = strip_think_tags(&text);

        Ok(text)
    }
}

impl GrammarResult {
    fn pass_through(text: String, start: Instant) -> Self {
        Self { success: true, corrected: text.clone(), original: text, duration_s: start.elapsed().as_secs_f64(), error: None }
    }
    fn fail(original: String, start: Instant, error: &str) -> Self {
        Self { success: false, corrected: original.clone(), original, duration_s: start.elapsed().as_secs_f64(), error: Some(error.into()) }
    }
}

/// Strip <think>...</think> tags from Qwen3 output.
/// Replaces the Python `think=False` parameter which isn't available in ollama-rs.
fn strip_think_tags(text: &str) -> String {
    // If text starts with <think>, find closing </think> and return everything after
    if let Some(rest) = text.strip_prefix("<think>") {
        if let Some(pos) = rest.find("</think>") {
            return rest[pos + 8..].trim().to_string();
        }
    }
    text.to_string()
}

fn parse_host_port(url: &str) -> (String, u16) {
    // Parse "http://localhost:11434" into ("http://localhost", 11434)
    // ollama-rs wants host and port separately
    todo!()
}
```

Key preservation from Python `grammar.py`:
- Fail-open contract: `corrected` field always contains safe-to-use text
- Length ratio guard: 0.5-1.5 range (Python `grammar.py:85-92`)
- Min-words skip: preserves trigger words from being rewritten (Python `grammar.py:59-60`)
- `think=False` equivalent: strip `<think>` tags from response

#### 2. Local Executor
**File**: `src/local_executor.rs`

```rust
use anyhow::Result;
use std::time::{Duration, Instant};
use tracing::{info, warn, error};

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub success: bool,
    pub response: String,
    pub error: Option<String>,
}

pub struct LocalExecutor {
    host: String,
    port: u16,
    model: String,
    timeout: Duration,
}

impl LocalExecutor {
    pub fn new(config: &crate::config::LocalConfig) -> Self {
        let (host, port) = crate::grammar::parse_host_port(&config.host);
        Self {
            host,
            port,
            model: config.model.clone(),
            timeout: Duration::from_secs_f64(config.timeout_s),
        }
    }

    /// Execute a prompt against the local Ollama model.
    /// NEVER raises — returns ExecutionResult with success=false on any error.
    pub async fn execute(&self, prompt: &str, model_override: Option<&str>) -> ExecutionResult {
        let model = model_override.unwrap_or(&self.model).to_string();
        let start = Instant::now();

        match self.call_ollama(prompt, &model).await {
            Ok(response) => {
                info!("Local execution in {:.3}s ({} chars)", start.elapsed().as_secs_f64(), response.len());
                ExecutionResult { success: true, response, error: None }
            }
            Err(e) => {
                let msg = classify_error(&e);
                error!("Local execution failed: {}", msg);
                ExecutionResult { success: false, response: String::new(), error: Some(msg) }
            }
        }
    }

    async fn call_ollama(&self, prompt: &str, model: &str) -> Result<String> {
        let ollama = ollama_rs::Ollama::new(&self.host, self.port);
        let request = ollama_rs::generation::completion::request::GenerationRequest::new(
            model.to_string(),
            prompt.to_string(),
        )
        .options(ollama_rs::generation::options::GenerationOptions::default()
            .num_predict(2048));

        let response = tokio::time::timeout(
            self.timeout,
            ollama.generate(request),
        ).await??;

        Ok(response.response.trim().to_string())
    }
}

/// Classify Ollama errors into user-friendly messages.
/// Port of local_executor.py:78-81
fn classify_error(e: &anyhow::Error) -> String {
    let msg = e.to_string().to_lowercase();
    if msg.contains("connection") || msg.contains("refused") {
        "Ollama is not running. Start it with: ollama serve".into()
    } else if msg.contains("not found") {
        "Model not found. Pull it with: ollama pull <model>".into()
    } else {
        e.to_string()
    }
}
```

#### 3. Ollama Auto-Start
**File**: `src/local_executor.rs` (additions)

```rust
/// Check if Ollama is running by hitting /api/tags.
/// Port of local_executor.py:114-122
pub async fn is_ollama_running(host: &str, port: u16) -> bool {
    let url = format!("{}:{}/api/tags", host, port);
    match tokio::time::timeout(
        Duration::from_secs(1),
        reqwest::get(&url), // or use ollama-rs's built-in check
    ).await {
        Ok(Ok(resp)) => resp.status().is_success(),
        _ => false,
    }
}

/// Start Ollama if not running. Poll until ready or timeout.
/// Port of local_executor.py:124-168
pub async fn ensure_ollama_running(host: &str, port: u16, max_wait_s: u64) -> bool {
    if is_ollama_running(host, port).await {
        return true;
    }

    info!("Starting Ollama server...");
    match tokio::process::Command::new("ollama")
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => {
            // Poll for readiness
            let deadline = Instant::now() + Duration::from_secs(max_wait_s);
            while Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if is_ollama_running(host, port).await {
                    info!("Ollama server ready");
                    return true;
                }
            }
            warn!("Ollama server did not start within {}s", max_wait_s);
            false
        }
        Err(e) => {
            error!("Failed to start Ollama: {}", e);
            false
        }
    }
}
```

**Note**: The `reqwest` dependency isn't in the Cargo.toml above. Either add it as a lightweight HTTP client for the health check, or use `ollama-rs`'s built-in connectivity check if available. Alternatively, use `std::net::TcpStream::connect` with a timeout for a zero-dependency health check.

#### 4. Wire Grammar + Executors into Pipeline
**File**: `src/agent.rs` (expand `stop_recording_and_process`)

After transcription, before routing:
```rust
// Grammar correction (fail-open)
let grammar_result = self.grammar.correct(&result.text).await;
let text = &grammar_result.corrected;

// Route
let route = crate::router::route(text);

// Dispatch
match route.route {
    RouteType::Type => {
        self.output.type_text(&route.text);
    }
    RouteType::Local => {
        self.notifier.processing(&self.config.local.model);
        let exec_result = self.local_executor.execute(&route.text, None).await;
        if exec_result.success {
            self.output.type_text(&exec_result.response);
        } else {
            self.notifier.error(&exec_result.error.unwrap_or_default());
        }
    }
    RouteType::Timer => {
        // Phase 7
        warn!("Timer route not yet implemented");
    }
    RouteType::Edit => {
        self.notifier.error("Edit route not implemented");
    }
    RouteType::Command => {
        self.notifier.error("Command route not implemented");
    }
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build --release` compiles
- [ ] `cargo test` passes:
  - Grammar: pass-through on disabled, pass-through on short text, length ratio rejection
  - `strip_think_tags`: correctly strips `<think>` wrapper, passes through clean text
  - Error classification: connection refused, model not found, generic error
- [ ] `cargo clippy` clean

#### Manual Verification:
- [ ] Grammar correction works: speak a sentence with bad grammar → corrected text is typed
- [ ] Grammar fail-open: stop Ollama → speak → original text is typed (no crash)
- [ ] "easy what is the capital of France" → routes to LOCAL → Ollama responds → response is typed
- [ ] Ollama auto-start: stop Ollama → start daemon → Ollama is started automatically
- [ ] Short phrases ("timer") are not grammar-corrected (min_words=3 bypass)

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 7: Timer + History + Deployment

### Overview
Wire up the timer executor (systemd-run subprocess), implement SQLite history logging, add media pause/resume via playerctl, update the systemd service file, and implement the `--check` dependency reporter.

### Changes Required:

#### 1. Timer Executor (full implementation)
**File**: `src/timer.rs` (add execute function to the module from Phase 1)

```rust
use std::process::Command;
use std::time::Instant;
use tracing::{info, error};

#[derive(Debug, Clone)]
pub struct TimerResult {
    pub success: bool,
    pub response: String,
    pub error: Option<String>,
}

pub struct TimerExecutor {
    sound_enabled: bool,
    sound_file: String,
}

impl TimerExecutor {
    pub fn new(config: &crate::config::TimerConfig) -> Self {
        Self {
            sound_enabled: config.sound_enabled,
            sound_file: config.sound_file.clone(), // tilde-expanded
        }
    }

    /// Execute a timer. NEVER raises.
    /// Port of timer_executor.py:177-272
    pub fn execute(&self, text: &str) -> TimerResult {
        let (seconds, remaining) = parse_duration(text);

        let seconds = match seconds {
            Some(s) if s > 0 => s,
            _ => return TimerResult {
                success: false,
                response: String::new(),
                error: Some(format!("Could not parse duration from: {}", text)),
            },
        };

        let label = if remaining.trim().is_empty() {
            "Timer complete".to_string()
        } else {
            remaining.trim().to_string()
        };

        let human_dur = format_human_duration(seconds);
        let systemd_dur = format_systemd_duration(seconds);

        // Build notify command (bash one-liner)
        let notify_cmd = if self.sound_enabled {
            format!(
                r#"SOUND_FILE="{}"; ( while true; do play -q "$SOUND_FILE" 2>/dev/null; sleep 1; done ) & SOUND_PID=$!; dunstify -a "Dictate Agent" -i alarm-symbolic -u critical -t 0 "Timer: {}" "{} elapsed" --action="default,Dismiss"; kill $SOUND_PID 2>/dev/null; wait $SOUND_PID 2>/dev/null"#,
                self.sound_file, label, human_dur
            )
        } else {
            format!(
                r#"dunstify -a "Dictate Agent" -i alarm-symbolic -u critical -t 0 "Timer: {}" "{} elapsed" --action="default,Dismiss""#,
                label, human_dur
            )
        };

        // Run systemd-run
        match Command::new("systemd-run")
            .args(["--user", &format!("--on-active={}", systemd_dur),
                   "--description=Dictate Agent Timer",
                   "/bin/bash", "-c", &notify_cmd])
            .output()
        {
            Ok(output) if output.status.success() => {
                info!("Timer set: {} ({})", human_dur, label);
                TimerResult {
                    success: true,
                    response: format!("Timer set for {}: {}", human_dur, label),
                    error: None,
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                TimerResult { success: false, response: String::new(), error: Some(stderr.to_string()) }
            }
            Err(e) => {
                TimerResult { success: false, response: String::new(), error: Some(e.to_string()) }
            }
        }
    }
}
```

#### 2. History Module
**File**: `src/history.rs`

```rust
use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::time::Instant;
use uuid::Uuid;
use chrono::Utc;
use tracing::{info, error};

const SCHEMA_VERSION: i32 = 1;
const DEFAULT_DB_DIR: &str = "dictate-agent";

pub struct HistoryStore {
    conn: Connection,
    session_id: String,
}

/// Mutable interaction builder — populated field-by-field across the pipeline.
/// Mirrors Python's Interaction dataclass at history.py:18-66.
pub struct Interaction {
    pub session_id: String,
    pub timestamp: String,
    start_time: Instant,

    // Audio
    pub audio_duration_s: Option<f64>,

    // Transcription
    pub raw_transcription: Option<String>,
    pub corrected_transcription: Option<String>,
    pub transcription_duration_s: Option<f64>,

    // Grammar
    pub grammar_input: Option<String>,
    pub grammar_output: Option<String>,
    pub grammar_changed: bool,
    pub grammar_error: Option<String>,
    pub grammar_duration_s: Option<f64>,

    // Routing
    pub route_type: Option<String>,
    pub route_model: Option<String>,
    pub route_trigger: Option<String>,
    pub route_confidence: Option<f64>,

    // Execution
    pub prompt_sent: Option<String>,
    pub response_text: Option<String>,
    pub execution_model: Option<String>,
    pub execution_duration_s: Option<f64>,
    pub execution_success: Option<bool>,
    pub execution_error: Option<String>,

    // Output
    pub output_typed: bool,
    pub output_char_count: Option<usize>,

    // Pipeline
    pub completed: bool,
    pub error_summary: Option<String>,
}

impl HistoryStore {
    pub fn new(config: &crate::config::HistoryConfig) -> anyhow::Result<Self> {
        let db_path = if config.db_path.is_empty() {
            default_db_path()
        } else {
            PathBuf::from(&config.db_path)
        };

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL")?;

        // Create tables — same schema as Python history.py:18-66
        conn.execute_batch(include_str!("../sql/schema.sql"))?;

        // Insert schema version if absent
        let count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))?;
        if count == 0 {
            conn.execute("INSERT INTO schema_version (version) VALUES (?1)", [SCHEMA_VERSION])?;
        }

        let session_id = Uuid::new_v4().to_string()[..12].to_string();
        info!("History store opened at {} (session {})", db_path.display(), session_id);

        Ok(Self { conn, session_id })
    }

    pub fn begin(&self) -> Interaction {
        Interaction {
            session_id: self.session_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            start_time: Instant::now(),
            // All other fields default to None/false
            ..Interaction::default()
        }
    }

    pub fn commit(&self, interaction: &Interaction) {
        let total_duration = interaction.start_time.elapsed().as_secs_f64();
        // INSERT INTO interactions (...) VALUES (...) — all 27 columns
        // Port of history.py:158-197
        if let Err(e) = self.insert(interaction, total_duration) {
            error!("Failed to commit interaction: {}", e);
        }
    }

    fn insert(&self, i: &Interaction, total_duration: f64) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO interactions (
                session_id, timestamp, audio_duration_s,
                raw_transcription, corrected_transcription, transcription_duration_s,
                grammar_input, grammar_output, grammar_changed, grammar_error, grammar_duration_s,
                route_type, route_model, route_trigger, route_confidence,
                prompt_sent, response_text, execution_model, execution_duration_s,
                execution_success, execution_error,
                output_typed, output_char_count,
                total_duration_s, completed, error_summary
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21,
                ?22, ?23, ?24, ?25, ?26
            )",
            params![
                i.session_id, i.timestamp, i.audio_duration_s,
                i.raw_transcription, i.corrected_transcription, i.transcription_duration_s,
                i.grammar_input, i.grammar_output, i.grammar_changed as i32,
                i.grammar_error, i.grammar_duration_s,
                i.route_type, i.route_model, i.route_trigger, i.route_confidence,
                i.prompt_sent, i.response_text, i.execution_model, i.execution_duration_s,
                i.execution_success.map(|b| b as i32), i.execution_error,
                i.output_typed as i32, i.output_char_count.map(|c| c as i64),
                total_duration, i.completed as i32, i.error_summary
            ],
        )?;
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }
}

fn default_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join(DEFAULT_DB_DIR)
        .join("history.db")
}
```

**File**: `sql/schema.sql` (new, same schema as Python `history.py:18-66`)

```sql
CREATE TABLE IF NOT EXISTS interactions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    audio_duration_s REAL,
    raw_transcription TEXT,
    corrected_transcription TEXT,
    transcription_duration_s REAL,
    grammar_input TEXT,
    grammar_output TEXT,
    grammar_changed INTEGER DEFAULT 0,
    grammar_error TEXT,
    grammar_duration_s REAL,
    route_type TEXT,
    route_model TEXT,
    route_trigger TEXT,
    route_confidence REAL,
    prompt_sent TEXT,
    response_text TEXT,
    execution_model TEXT,
    execution_duration_s REAL,
    execution_success INTEGER,
    execution_error TEXT,
    output_typed INTEGER DEFAULT 0,
    output_char_count INTEGER,
    total_duration_s REAL,
    completed INTEGER DEFAULT 0,
    error_summary TEXT
);

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL
);
```

**Backward compatibility**: The Rust version writes to the same `history.db` with the same schema. Existing interaction data from the Python daemon is preserved. Both can read the same database.

#### 3. Media Pause/Resume
**File**: `src/agent.rs` (add methods)

```rust
/// Check if media is playing via playerctl.
/// Port of main.py:325-335
fn is_media_playing() -> bool {
    Command::new("playerctl")
        .arg("status")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "Playing")
        .unwrap_or(false)
}

fn pause_media() {
    let _ = Command::new("playerctl").arg("pause").output();
}

fn resume_media() {
    let _ = Command::new("playerctl").arg("play").output();
}
```

Media state file logic (matching `main.py:161-165`):
- On start_recording: if playing, write `media_state_path()`, then pause
- On pipeline complete: if state file exists, resume, delete state file
- State file persists across crashes — next recording detects it

#### 4. Dependency Checker
**File**: `src/main.rs` (expand `check_all_dependencies`)

Check each external program that's still used as a subprocess:
- `playerctl` — media control
- `systemd-run` — timer creation
- `dunstify` — persistent timer notifications
- `play` (sox) — timer alarm sound
- `ollama` — auto-start

Report format matches Python output: `[OK]` or `[MISSING]` with install hints.

#### 5. Update Systemd Service
**File**: `systemd/dictate-agent.service`

```ini
[Unit]
Description=Dictate Agent - Voice Dictation Daemon
After=graphical-session.target
Wants=pulseaudio.service

[Service]
Type=simple
ExecStart=%h/dictate_agent/target/release/dictate-agent
Restart=on-failure
RestartSec=5
Environment=DISPLAY=:0
Environment=DICTATE_AGENT_LOG=dictate_agent=info
MemoryMax=4G
CPUQuota=80%

[Install]
WantedBy=default.target
```

Changes from current service file:
- Remove `RUST_LOG` → use `DICTATE_AGENT_LOG` (or keep `RUST_LOG` for tracing-subscriber compatibility)
- Increase `MemoryMax=2G` → `4G` (whisper.cpp GGUF model + VRAM management)
- Increase `CPUQuota=50%` → `80%` (cpal audio processing needs more headroom)
- Remove `network.target` from After (not needed — Ollama is localhost)
- ExecStart path already correct: `%h/dictate_agent/target/release/dictate-agent`

#### 6. Update Config Example
**File**: `config/config.example.toml`

Replace with the cleaned-up schema documented in the "Config Schema Cleanup" section above.

#### 7. Update Helper Scripts
**Files**: `scripts/dictate-toggle`, `scripts/dictate-cancel`

No changes needed — they send signals to the PID, which works identically with the Rust binary.

**File**: `scripts/run.sh`

Update to run the Rust binary directly (no venv activation needed):
```bash
#!/bin/bash
exec ~/dictate_agent/target/release/dictate-agent
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build --release` compiles
- [ ] `cargo test` passes all tests (Phases 1-7)
- [ ] `cargo clippy` clean
- [ ] `./target/release/dictate-agent --check` reports all dependencies
- [ ] Binary size is under 50MB (expected ~15-25MB with LTO + strip)
- [ ] `sql/schema.sql` matches Python `history.py` schema exactly (diff-able)

#### Manual Verification:
- [ ] **Full pipeline**: signal → record → transcribe → grammar → route → type
- [ ] **Timer**: "timer 5 minutes test" → systemd timer created, fires with notification + sound
- [ ] **Local**: "easy what is Rust" → Ollama responds → response typed
- [ ] **History**: interactions appear in `~/.local/share/dictate-agent/history.db`
- [ ] **Media**: music pauses on recording start, resumes after pipeline completes
- [ ] **Systemd**: `systemctl --user start dictate-agent` works with updated service file
- [ ] **Config migration**: old config.toml from Python version loads (unknown keys ignored by serde)
- [ ] **Cold start**: daemon starts, model loads in ~3-5s (log timing)
- [ ] **Performance**: transcription latency ~400-600ms (log timing, compare to Python's ~183ms)
- [ ] **Memory**: steady-state RSS under 3GB (GGUF model + runtime)

**Implementation Note**: After completing all phases, do a final end-to-end regression test covering every route type and error path. Then update CLAUDE.md to reflect the new Rust codebase.

---

## Testing Strategy

### Unit Tests (cargo test):
- **Config**: default values, TOML parsing with missing sections, tilde expansion, invalid TOML error
- **Router**: all 5 route types, edge cases (empty, punctuation, case insensitivity)
- **Duration parser**: numeric ("5m"), word ("five minutes"), compound ("1 hour 30 minutes"), half-hour variants, fallback search, no-match returns None
- **Systemd/human duration formatting**: edge cases (0, 59, 60, 3661)
- **Grammar**: `strip_think_tags` function, length ratio bounds
- **Corrections table**: all 14 Whisper correction pairs

### Integration Tests (manual, per-phase):
- Phase 2: signal send/receive via kill
- Phase 3: audio capture produces non-empty buffer
- Phase 4: transcription returns text from speech
- Phase 5: text appears in focused window
- Phase 6: Ollama round-trip works
- Phase 7: timer fires, history row inserted

### No Automated Integration Tests:
The Python version has no test suite. The Rust version adds unit tests but integration tests require hardware (microphone, GPU, X11 display, Ollama server) that can't run in CI. Manual verification per phase is the test strategy.

## Performance Considerations

| Metric | Python (current) | Rust (expected) | Notes |
|--------|-----------------|-----------------|-------|
| Transcription latency | ~183ms avg | ~400-600ms | whisper.cpp vs HF transformers; still sub-second |
| Model load time | ~15-30s | ~3-5s | GGUF format loads faster |
| Cold start to ready | ~20-35s | ~5-8s | No Python interpreter, no torch import |
| Binary size | ~2GB (venv) | ~15-25MB | Single binary vs Python + torch + transformers |
| VRAM usage | ~5GB (FP16) | ~2.5GB (GGUF) | Significant reduction |
| RAM usage | ~500MB | ~100MB | No Python runtime overhead |
| Pipeline total | ~870ms avg | ~700-900ms | Transcription slower, everything else faster |

## Migration Notes

1. **Config migration**: The Rust binary reads `~/.config/dictate-agent/config.toml`. Users with existing Python configs will see unknown keys ignored (serde default behavior). New keys use defaults. Recommend copying the new `config.example.toml` and adjusting.

2. **GGUF model download**: Users must download the Whisper model manually:
   ```bash
   mkdir -p ~/.local/share/dictate-agent/models
   wget -O ~/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin \
     https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
   ```

3. **History DB**: Same SQLite schema, same file path. Existing data preserved. Both Python and Rust versions can read/write the same database.

4. **Systemd service**: ExecStart already points to `target/release/dictate-agent`. After building, just `systemctl --user daemon-reload && systemctl --user restart dictate-agent`.

5. **Python cleanup** (after Rust version is stable): Remove `dictate/` directory, `.venv/`, `pyproject.toml`. Keep `src_rust_archive/` as historical reference.

6. **Build dependencies**: The Rust build requires:
   - Rust toolchain (rustup)
   - CUDA toolkit (for whisper-rs GPU support)
   - PipeWire development headers (for cpal PipeWire feature)
   - X11 development headers (for enigo x11rb feature)
   - SQLite3 is bundled (rusqlite bundled feature)

## References

- Research document: `thoughts/shared/research/2026-04-10-rust-go-rewrite-feasibility.md`
- Original project spec: `thoughts/shared/project/2026-01-15-dictate-agent.md`
- Python completion report: `thoughts/shared/project/2026-01-15-dictate-agent-completion.md`
- Archived Rust prototype: `src_rust_archive/`
- Current Python source: `dictate/`
- whisper-rs documentation: https://github.com/tazz4843/whisper-rs (now on Codeberg)
- GGUF model: https://huggingface.co/ggerganov/whisper.cpp
