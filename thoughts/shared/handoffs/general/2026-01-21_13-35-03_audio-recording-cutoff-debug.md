---
date: 2026-01-21T13:35:03-08:00
researcher: Claude
git_commit: e99ea3467f3dae98e443302c05c0d977fdac816d
branch: master
repository: dictate_agent
topic: "Audio Recording Cutoff Bug Investigation"
tags: [debugging, audio, parecord, pw-record, pipewire]
status: in_progress
last_updated: 2026-01-21
last_updated_by: Claude
type: debug_investigation
---

# Handoff: Audio Recording Cutoff Debug

## Task(s)
**Status: In Progress**

Investigating why audio recordings get cut off towards the end. Jake reported that recordings can be truncated.

## Critical References
- `dictate/audio.py` - Core audio recording logic using `parecord`
- `dictate/main.py:114-131` - Stop recording flow

## Recent changes
None - this was a debugging/investigation session, no code changes made.

## Learnings

### Root Cause Identified
The audio cutoff is caused by **buffered audio data being lost when SIGINT is sent to parecord**. Key findings:

1. **The Issue**: `parecord` uses a 50ms latency buffer. When SIGINT is sent, audio data still in the buffer is NOT written to the file - it's simply discarded.

2. **Measured Impact**: Testing showed 50-100ms of audio is typically lost (intermittent, ~25% of recordings affected):
   - `dictate/audio.py:65` sends SIGINT
   - `parecord` exits without flushing its internal buffer
   - The post-stop 100ms sleep (`audio.py:71`) doesn't help because the data is already lost

3. **Buffer Lag Confirmed**: File monitoring showed the written data consistently lags ~100ms (3200 bytes at 16kHz stereo) behind actual recording time.

### Tested Solutions

| Approach | Result |
|----------|--------|
| Current (SIGINT + 100ms delay) | max=100ms cutoff, 25% failure rate |
| SIGTERM instead of SIGINT | Slightly better but still fails |
| Longer post-stop delay (200ms) | Does not help (data already lost) |
| Different `--latency-msec` values | 10ms-100ms all have similar issues |
| `--process-time-msec` parameter | No significant improvement |
| Using `parec` with raw output + pipe | Still loses ~10-50ms |
| Draining pipe after stop | Buffer not fully accessible |

### Promising Alternative: `pw-record`
Testing `pw-record` (native PipeWire tool) showed **significantly better results**:
- Much lower cutoff (typically <10ms vs 50-100ms)
- More consistent behavior
- Test was interrupted before completion but early results were very promising

### Technical Details
- System: PipeWire 1.4.10 with PulseAudio compatibility layer
- Current command: `parecord --format=s16le --rate=16000 --channels=1 --file-format=wav --latency-msec=50`
- Potential replacement: `pw-record --rate=16000 --channels=1 --latency=10ms`

## Artifacts
None created - debugging session only.

## Action Items & Next Steps

1. **Complete `pw-record` testing**: Run the interrupted comparison test to verify `pw-record` is a reliable fix:
   ```python
   # Compare parecord vs pw-record with 10+ runs each
   # Measure max cutoff, average cutoff, and failure rate
   ```

2. **Implement fix in `audio.py`**: If `pw-record` proves reliable, update `dictate/audio.py`:
   - Replace `parecord` command with `pw-record`
   - Adjust parameters: `--rate=16000 --channels=1 --latency=10ms`
   - Update dependency check in `check_audio_dependencies()`

3. **Fallback strategy**: Consider keeping `parecord` as fallback for systems without `pw-record`

4. **Test edge cases**: Test with various recording durations (short <1s, long >30s) to ensure fix is comprehensive

## Other Notes

### Key Code Locations
- `dictate/audio.py:40-52` - parecord subprocess launch
- `dictate/audio.py:57-73` - stop recording logic (where cutoff occurs)
- `dictate/audio.py:82-89` - dependency check (needs update for pw-record)

### Test Command Used
```python
# Quick test to verify cutoff behavior
python3 -c "
import subprocess, signal, time, tempfile, wave
from pathlib import Path

fd = tempfile.NamedTemporaryFile(suffix='.wav', delete=False)
fd.close()
temp_file = Path(fd.name)

process = subprocess.Popen(['parecord', '--format=s16le', '--rate=16000',
    '--channels=1', '--file-format=wav', '--latency-msec=50', str(temp_file)],
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2)
process.send_signal(signal.SIGINT)
process.wait()
time.sleep(0.1)

with wave.open(str(temp_file), 'rb') as wf:
    duration = wf.getnframes() / wf.getframerate()
    print(f'Expected: 2.0s, Actual: {duration:.3f}s, Cutoff: {(2.0-duration)*1000:.0f}ms')
temp_file.unlink()
"
```

### pw-record Command Syntax
```bash
# Native PipeWire recording - appears to have better buffer handling
pw-record --rate=16000 --channels=1 --latency=10ms output.wav
```
