# Architecture Reference

## Module Dependency Graph

```
config.py (leaf — imported by transcribe.py, router.py, and main.py)
    │
    ├── WhisperConfig → transcribe.py
    ├── RouterConfig  → router.py
    └── Config        → main.py (load_config)

main.py (hub — imports ALL other modules)
    ├── audio.py           (no internal imports)
    ├── transcribe.py      (imports config.WhisperConfig)
    ├── grammar.py         (no internal imports)
    ├── router.py          (imports config.RouterConfig)
    ├── executor.py        (no internal imports)
    ├── local_executor.py  (no internal imports)
    ├── timer_executor.py  (no internal imports)
    ├── output.py          (no internal imports)
    └── notify.py          (no internal imports)
```

**Rule**: New modules should NOT import from sibling modules. Accept config values through constructor parameters, passed from `main.py`.

## DictateAgent Initialization (main.py:42-83)

Components are created in this order:
1. `load_config()` — loads TOML
2. `Notifier` — receives notification config values
3. `AudioCapture` — no config needed
4. `OutputHandler` — receives typing_delay_ms, auto_type
5. `Router` — receives RouterConfig dataclass
6. `ClaudeExecutor` — receives default model name
7. `ensure_ollama_running()` — starts Ollama if not running
8. `GrammarCorrector` — receives host (from router), model/timeout/enabled/min_words (from grammar config)
9. `LocalExecutor` — receives host, model, timeout
10. `TimerExecutor` — no config needed
11. `Transcriber` — receives WhisperConfig, on_ready/on_error callbacks

## Signal Handling

### Registration (main.py:358-371)

```python
signal.signal(signal.SIGINT, handle_sigint)    # → agent.stop()
signal.signal(signal.SIGTERM, handle_sigint)   # → agent.stop()
signal.signal(signal.SIGUSR1, handle_sigusr1)  # → agent.toggle()
signal.signal(signal.SIGUSR2, handle_sigusr2)  # → agent.cancel()
```

### Main Loop (main.py:298-300)

```python
while self.running:
    signal.pause()  # Blocks until signal received
```

### PID File Lifecycle
- Written at startup: `main.py:353`
- Read by scripts: `scripts/dictate-toggle`, `scripts/dictate-cancel`
- Deleted on shutdown: `main.py:277-278`
- Location: `~/.config/dictate-agent/dictate.pid`

## Media State Tracking

Prevents audio conflicts during recording/transcription:

1. `_start_recording()` checks `playerctl status` (main.py:122)
2. If playing, writes "playing" to `MEDIA_STATE_FILE` and pauses (main.py:123-124)
3. After processing completes, `_resume_media_if_needed()` checks file existence (main.py:262-269)
4. Deletes state file and resumes playback

File location: `~/.config/dictate-agent/media_was_playing`

## Audio Recording Lifecycle

1. `AudioCapture.start()` — flushes stale buffer via brief `parecord --raw`, then starts recording subprocess
2. Recording writes to temp WAV: `tempfile.NamedTemporaryFile(suffix=".wav")`
3. `AudioCapture.stop()` — sleeps 0.5s for trailing audio, sends SIGINT to parecord, waits for exit
4. `AudioCapture.cleanup()` — deletes temp file

**Known issue**: parecord can lose 50-100ms of audio at end due to buffer not flushing on SIGINT. `pw-record` identified as potential fix but not yet implemented. See `thoughts/shared/handoffs/general/2026-01-21_13-35-03_audio-recording-cutoff-debug.md`.

## Transcription Pipeline

- Models loaded asynchronously in daemon thread (transcribe.py:68-71)
- `threading.Event` synchronizes readiness (transcribe.py:61)
- Uses HuggingFace `pipeline("automatic-speech-recognition")` with chunking for >30s audio
- Speculative decoding incompatible with chunked pipeline (see `thoughts/shared/research/2026-01-30-whisper-long-form-transcription.md`)
- Duration calculated from WAV file size: `(file_size - 44) / (16000 * 2)`
- Applies text corrections for common Whisper misrecognitions (e.g., "clod" → "claude")

## Grammar Correction Pipeline Step (main.py:172-179)

After transcription and before routing, text passes through `GrammarCorrector.correct()`:
- **Fail-open**: On any error, returns original text unchanged (never blocks pipeline)
- **Short text bypass**: Skips text under `min_words` (default 3) to preserve trigger words
- **Length ratio guard**: Falls back to original if corrected text is >150% or <50% of original length
- Uses Ollama `generate()` with `think=False` to disable Qwen3 chain-of-thought mode
- Controlled by `[grammar]` section in config TOML

## Route Dispatch (main.py:191-251)

The `_handle_route()` method switches on `RouteType` enum:

| Route | Handler | Output |
|-------|---------|--------|
| TYPE | `output.type_text()` | Types text directly |
| HAIKU/SONNET/OPUS | `executor.execute()` | Runs claude CLI, types response |
| LOCAL | `local_executor.execute()` | Runs ollama, types response |
| TIMER | `timer_executor.execute()` | Creates systemd timer |
| EDIT | (prints "not yet implemented") | Falls through to typing |
| COMMAND | (prints "not yet implemented") | Falls through to typing |
