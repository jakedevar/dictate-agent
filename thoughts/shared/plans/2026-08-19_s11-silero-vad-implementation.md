---
date: 2026-08-19
slice: S11
status: implemented
---

# S11 — Silero VAD implementation plan

## Scope

Build `dictate-vad` with the CPU-capable `voice_activity_detector` Silero v5
backend. Gate no-speech before STT, trim leading/trailing silence, and make
`one_shot` sessions stop after configured trailing silence. Preserve S02's
single-writer engine and do not change STT/model management.

## Design

1. `dictate-vad` owns VAD configuration, the synchronous gate/trim operation,
   and a stateful trailing-silence tracker. It operates on 16 kHz mono f32
   audio in Silero's required 512-sample windows.
2. `dictate-core` gains a narrow VAD port and a non-disruptive audio snapshot
   seam. The daemon's existing host capture implementation answers snapshots
   from its current buffer; mocks remain deterministic.
3. The pipeline polls the snapshot seam only for `one_shot`/`wake_word` modes.
   The normal VAD stage runs after capture flush, records a real duration, and
   either returns trimmed samples or skips STT with `no_speech_detected`.
4. Fixture tests use committed speech/silence/quiet WAVs and exercise the
   Silero decision path. Core tests prove no-speech never calls STT, trim
   reaches STT, and one-shot stops without a protocol Stop.

## Verification

- `cargo test -p dictate-vad`
- `cargo test -p dictate-core`
- CUDA-env workspace test and clippy
- Record an in-process VAD timing delta on fixtures; hardware microphone and
  real spoken-word calibration remain manual checks.

## Result

The fixture measurement was 113.23 ms CPU VAD for a 33.27 s speech WAV,
reducing the STT input to 31.34 s (1.93 s / 5.8% trimmed). This confirms the
pre-STT trim accounting path, not end-to-end microphone-to-injection latency.
