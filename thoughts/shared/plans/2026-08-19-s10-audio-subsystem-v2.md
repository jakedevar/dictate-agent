---
date: 2026-08-19
slice: S10
status: complete
---

# S10 — Audio subsystem v2

## Scope delivered

- Keep a 16 kHz bounded ring buffer armed while idle and prepend its 300 ms
  snapshot when a recording starts.
- Select a configured CPAL input device by exact name or use the system
  default; mark stream errors and safely reopen on the next start for hotplug
  recovery.
- Apply bounded RMS gain to quiet speech without amplifying near-silence;
  retain raw diagnostics for mute detection.
- Add optional Rodio start/stop/cancel/error earcons, preserving the previous
  silent behavior by default.
- Preserve existing playerctl pause/resume semantics and publish additive
  `audio_activity` protocol events for earcons, media actions, input recovery,
  and a mic-mute warning.

## Decisions

- VAD and silence trimming remain S11 work. Mute detection is warning-only;
  it neither rejects capture nor skips STT.
- Audio output, playerctl, and device recovery are best-effort and have no
  authority to fail an otherwise valid session.
- Earcons default off; AGC defaults on with an 8x ceiling, as it affects only
  audio passed to STT and is the requested quiet-speech parity feature.

## Verification manifest

## Recovery scope audit

Recovered from the read-only interrupted sandbox: the audio crate's bounded
pre-roll capture, exact-device selection and next-start recovery, AGC/mute
diagnostics, Rodio earcons, audio configuration, the additive protocol event,
pipeline media/earcon/mute event publishing, production wiring, and the UDS
event-stream test.

Deliberately discarded as out of scope: CLI command and formatting changes
(except the required `audio_activity` renderer arm);
core cancellation, engine, and session formatting; all history changes;
unrelated protocol command/envelope/error/frame/record/result and compatibility
test churn; daemon paths/server/signals churn; and every formatting-only hunk
outside the retained S10 integration files. The retained integration files
contain only S10 behavior plus the minimal local formatting needed to carry it.

### AUTOMATED

- `cargo test -p dictate-audio -p dictate-core -p dictate-proto`: audio ring,
  gain fixture, device selection, recovery/mute decisions, and protocol tests.
- `cargo test -p dictated --test control_plane audio_side_effects_are_visible_in_the_daemon_event_stream`:
  verifies start/stop earcons and media pause/resume are observable over UDS.
- Full workspace tests and clippy are run before handoff.

### DAEMON

- With `[audio.earcons] enabled = true`, run `dictate tail`, dictate once, and
  confirm `audio_activity` events for `earcon_start`, `earcon_stop`, and any
  `media_paused`/`media_resumed` action.
- Unplug/replug a selected USB microphone between sessions and confirm the next
  start either recovers with `device_recovered` or returns an actionable input
  device error.

### MANUAL

- In a focused app, speak immediately after the hotkey and confirm the first
  syllable is retained; speak quietly and confirm recognition improves without
  clipping.
- Mute the microphone for at least 900 ms and confirm the desktop warning and
  `microphone_muted` event. Requires a real mic, PipeWire/ALSA, desktop
  notifications, and an audio output device.
