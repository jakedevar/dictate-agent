---
date: 2026-01-23T13:03:27-08:00
researcher: Claude
git_commit: e99ea3467f3dae98e443302c05c0d977fdac816d
branch: master
repository: dictate_agent
topic: "Interruption Keybind for Recording Cancel"
tags: [research, codebase, signal-handling, keybinds, audio-recording]
status: complete
last_updated: 2026-01-23
last_updated_by: Claude
---

# Research: Interruption Keybind for Recording Cancel

**Date**: 2026-01-23T13:03:27-08:00
**Researcher**: Claude
**Git Commit**: e99ea3467f3dae98e443302c05c0d977fdac816d
**Branch**: master
**Repository**: dictate_agent

## Research Question

How to add an interruption keybind to kill a recording session without triggering the transcription step?

## Summary

The current implementation uses a single signal (`SIGUSR1`) to toggle recording on/off. Adding an interruption keybind requires adding a **second signal handler** (e.g., `SIGUSR2`) that cancels recording and discards audio without transcription. The architecture already supports this cleanly—it would involve adding a new method to `DictateAgent` and a corresponding toggle script.

## Current Signal Architecture

### Signal Flow
```
User presses $mod+n → scripts/dictate-toggle → kill -USR1 <pid> → DictateAgent.toggle()
```

### Registered Signals (`main.py:328-336`)
| Signal | Handler | Purpose |
|--------|---------|---------|
| `SIGINT` | `handle_sigint` → `agent.stop()` | Graceful daemon shutdown |
| `SIGTERM` | `handle_sigint` → `agent.stop()` | Graceful daemon shutdown |
| `SIGUSR1` | `handle_sigusr1` → `agent.toggle()` | Toggle recording on/off |

### Current Toggle Logic (`main.py:88-148`)
```python
def toggle(self) -> None:
    """Toggle recording on/off."""
    if self.recording:
        self._stop_recording()  # <-- Calls transcription
    else:
        self._start_recording()
```

When `toggle()` is called while recording, `_stop_recording()`:
1. Sets `self.recording = False`
2. Calls `self.audio.stop()` to terminate parecord subprocess
3. **Transcribes** via `self.transcriber.transcribe(audio_path)`
4. Routes and executes the transcribed text
5. Resumes media if it was paused

### Audio Recording Process (`audio.py`)
- Recording is managed by `AudioCapture` dataclass
- `start()` launches `parecord` subprocess writing to temp WAV file
- `stop()` sends `SIGINT` to parecord and returns the file path
- `cleanup()` deletes the temp file

## What Would Be Needed for Interruption

### 1. New Cancel Method on `DictateAgent` (`main.py`)

A new method that stops recording **without** transcription:

```python
def cancel(self) -> None:
    """Cancel recording without transcription."""
    if not self.recording:
        return

    self.recording = False
    self.audio.stop()      # Stop parecord
    self.audio.cleanup()   # Delete temp file immediately

    # Resume media if it was playing
    self._resume_media_if_needed()

    self.notifier.notify("Recording Cancelled", "Discarded", "dialog-cancel", 2000)
```

### 2. New Signal Handler (`main.py:main()`)

Add `SIGUSR2` handler for cancel:

```python
def handle_sigusr2(sig, frame):
    agent.cancel()

signal.signal(signal.SIGUSR2, handle_sigusr2)
```

### 3. New Toggle Script (`scripts/dictate-cancel`)

```bash
#!/bin/bash
# Cancel recording for dictate-agent daemon (no transcription)
# Send SIGUSR2 to cancel

PID_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/dictate-agent/dictate.pid"

if [ ! -f "$PID_FILE" ]; then
    notify-send -a "Dictate Agent" "Not running" "Start with: dictate-agent"
    exit 1
fi

PID=$(cat "$PID_FILE")

if ! kill -0 "$PID" 2>/dev/null; then
    notify-send -a "Dictate Agent" "Not running" "Process $PID not found"
    rm -f "$PID_FILE"
    exit 1
fi

# Send SIGUSR2 to cancel recording
kill -USR2 "$PID"
```

### 4. i3 Keybind

Add to i3 config (suggested: `$mod+Shift+n` as cancel variant):

```
bindsym $mod+Shift+n exec ~/dictate_agent/scripts/dictate-cancel
```

## Alternative Approaches

### Option A: Double-tap to cancel
Instead of a separate keybind, detect rapid double-tap of `$mod+n`:
- If `toggle()` is called twice within ~300ms while recording, treat as cancel
- Requires state tracking of last signal time
- Mentioned in completion report as "double-tap" option

### Option B: Long-press detection
Use i3's `--release` mode to detect held key:
- `bindsym --release $mod+n` could behave differently from tap
- Adds complexity to scripts

### Option C: Escape key in shell script
Have the toggle script listen for Escape during recording:
- More complex implementation
- Doesn't work well with signal-based architecture

**Recommendation**: Option A (separate `SIGUSR2` keybind) is cleanest and most consistent with existing architecture.

## Code References

| File | Lines | Description |
|------|-------|-------------|
| `dictate/main.py` | 37-74 | `DictateAgent.__init__()` - component initialization |
| `dictate/main.py` | 88-94 | `toggle()` - current toggle implementation |
| `dictate/main.py` | 95-112 | `_start_recording()` - starts parecord |
| `dictate/main.py` | 114-150 | `_stop_recording()` - stops and transcribes |
| `dictate/main.py` | 232-240 | `_resume_media_if_needed()` - media playback logic |
| `dictate/main.py` | 328-336 | Signal handler registration |
| `dictate/audio.py` | 19-55 | `AudioCapture.start()` - launches parecord |
| `dictate/audio.py` | 57-76 | `AudioCapture.stop()` - terminates parecord |
| `dictate/audio.py` | 78-82 | `AudioCapture.cleanup()` - deletes temp file |
| `scripts/dictate-toggle` | 1-21 | Toggle script (SIGUSR1) |
| `config/config.example.toml` | 1-76 | Configuration options |

## Historical Context (from thoughts/)

### Completion Report (`thoughts/shared/project/2026-01-15-dictate-agent-completion.md`)
- Line 137 mentions "Add interrupt mechanism (escape key or double-tap)" as a suggested next step
- Confirms this feature was considered but deferred from MVP

### Audio Cutoff Debug (`thoughts/shared/handoffs/general/2026-01-21_13-35-03_audio-recording-cutoff-debug.md`)
- Documents investigation into parecord buffer issues
- Relevant if cancel needs to ensure clean subprocess termination
- `pw-record` was identified as potential alternative with better buffer handling

## Implementation Complexity

| Component | Effort | Notes |
|-----------|--------|-------|
| `cancel()` method | ~10 lines | Similar to existing stop logic |
| Signal handler | ~3 lines | Copy pattern from SIGUSR1 |
| Cancel script | ~20 lines | Copy from dictate-toggle |
| i3 keybind | 1 line | User configuration |
| **Total** | ~35 lines | Low complexity |

## Open Questions

1. **Notification style**: Should cancel show a toast, or be silent for faster recovery?
2. **Media resume**: Should cancelled recording resume media, or leave it paused?
3. **Keybind choice**: `$mod+Shift+n` vs `$mod+Escape` vs other?
