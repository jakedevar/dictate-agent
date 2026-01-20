# Phase 1 Implementation Plan: Core Pipeline (MVP)

## Overview

Implement the foundational voice-to-text-to-Claude pipeline: signal-based daemon, audio recording, Whisper transcription, keyword routing, Claude Code execution, and streamed text typing.

## Changes Required

### 1. src/main.rs
Entry point with tokio runtime and module declarations.

### 2. src/config.rs
Configuration struct loaded from TOML.

### 3. src/notify.rs
Notification helper functions for recording/processing/responding states.

### 4. src/capture.rs
Audio capture using cpal with start/stop control.

### 5. src/router.rs
Keyword-based model routing (Haiku/Sonnet/Opus selection).

### 6. src/executor.rs
Claude Code subprocess management with NDJSON stream parsing.

### 7. src/output.rs
Text typing via enigo with configurable delay.

### 8. src/transcribe.rs
Whisper transcription using candle-transformers.

### 9. src/agent.rs
Main daemon orchestration: signal handling, state machine, pipeline coordination.

## Success Criteria

- [ ] `cargo build --release` compiles successfully
- [ ] `cargo test` passes
- [ ] Daemon starts and writes PID file
- [ ] SIGUSR1 toggles recording state
- [ ] Audio captured to buffer during recording
- [ ] Whisper transcribes audio to text
- [ ] Router selects appropriate Claude model
- [ ] Claude Code executes and streams response
- [ ] Response typed into active window
- [ ] Toast notifications show state changes
- [ ] SIGTERM/SIGINT triggers graceful shutdown

## Implementation Order

1. config.rs - Configuration foundation
2. notify.rs - Notification helpers
3. router.rs - Model selection logic
4. output.rs - Text typing
5. executor.rs - Claude Code integration
6. capture.rs - Audio recording
7. transcribe.rs - Whisper transcription
8. agent.rs - Main orchestration
9. main.rs - Entry point

## Manual Verification (Deferred)

- [ ] Press $mod+m to start recording
- [ ] Speak a test request
- [ ] Press $mod+m to stop recording
- [ ] Verify transcription accuracy
- [ ] Verify appropriate model selected
- [ ] Verify response typed into window
