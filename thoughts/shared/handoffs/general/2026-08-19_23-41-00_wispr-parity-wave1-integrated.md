---
title: Wispr parity Wave 1 integrated and verified
date: 2026-08-19
branch: rsi/4ba1578b
status: complete
---

# Outcome

Wave 1 is integrated on the RSI-owned branch `rsi/4ba1578b`. No branch was
created or changed, nothing was pushed, and the Python reference daemon paths
remain isolated from the Rust daemon. The similarly worded RSI session titles
were a shared sandbox-safety preamble; the sessions owned distinct slices.

The initial worker cohort was interrupted together by an RSI daemon restart.
Recovery workers salvaged and completed the separate slice branches, which
were reviewed and merged here.

# Integrated work

- Atomic protocol `Toggle`: one daemon request decides start-vs-stop, including
  concurrent toggle coverage.
- S10 audio v2: pre-roll ring, device selection/recovery, bounded AGC, mute
  diagnostics, optional earcons, media events, mono preference/downmix, and
  cancellation residue clearing.
- S11 VAD: Silero CPU gate/trim, no-speech STT skip, hands-free trailing-silence
  stop, golden WAV coverage, and explicit-stop fallback when snapshots fail.
- S12 STT v2: provider/request contract, pinned checksum-verified resumable
  model catalog, model CLI, language/prompt hooks, timings, CPU test feature,
  and real RTX 5080 benchmark evidence. The measured large-v3-turbo CUDA decode
  was p50 207.300 ms / p95 278.351 ms with 3,185.551 ms cold load for a 7.949 s
  fixture. R2 streaming remains NO-GO/deferred because the current API is not
  incremental and the contention margin is unsafe.
- S13 injection v2: async injector boundary, hardened X11 paste/type policies,
  Unicode chunking, direct-typing fallback, Xvfb+xterm smoke, and a Wayland
  portal consent stub. Non-text clipboards are not overwritten, and a failed
  clipboard restore after successful paste cannot cause duplicate delivery.
- S30 history/privacy v2: migrated schema, FTS5, analytics, retention, no-store
  mode, protocol purge/analytics, and optional one-time Python DB import.
  Imports are transactional; explicit purge uses secure delete, WAL truncate,
  and VACUUM.
- S31 hotkeys: passive evdev hold/toggle/cancel, release debounce,
  double-tap-to-lock, graceful permission degradation, shutdown joining, and
  uinput-to-real-engine transition coverage. Raw EVIOCGRAB was removed because
  it would suppress the entire selected keyboard in X11/Wayland.

# Lead corrections after review

- Preserved S10 audio and S11 VAD behavior while resolving shared pipeline and
  port conflicts.
- Kept the `dictate` control CLI free of whisper.cpp/CUDA by feature-gating STT
  transcription support; the daemon explicitly enables CUDA.
- Added bounded model-download connection and transfer timeouts.
- Made history imports atomic with their idempotency marker and hardened purge
  against recoverable SQLite/WAL remnants.
- Corrected the hotkey double-tap window (previously limited by the shorter
  release grace), removed whole-device exclusive grabs, and made queue drops
  visible.
- Updated deprecated Ollama client construction and formatted the integrated
  workspace.

# Verification

The final branch passed:

- `cargo test --workspace --all-targets` — 419 passed, 0 failed.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test -p dictate-stt --no-default-features --features cpu-tiny-ci` — 8
  passed.
- `cargo tree -p dictate-cli -e normal | rg 'whisper|dictate-fmt'` — no matches,
  confirming the hot-path control CLI does not link the transcription stack.
- X11 injection smoke passed under isolated Xvfb/xterm.
- Daemon control-plane suite passed all 44 cases.

# Commit landmarks

- `bb69c79` S11 correction
- `4d2fd63` atomic Toggle merge
- `3deda71` / `6de1bee` S10 merge and correction
- `7805efd` / `100499b` S13 merge and correction
- `83ad028` / `011d770` S12 merge and correction
- `8f08807` / `b3c929f` S30 merge and correction
- `13f84a7` S31 merge plus lead corrections
- `53fcda0` final formatting and warning-free quality gate

# Next

Wave 2 can now fan out: S20 formatting rules, S21 formatting LLM/eval harness,
S22 dictionary, S23 app context, S24 snippets, and S25 command/edit mode. Do
not open S32 yet; its dictionary dependency S22 is not complete. Keep R2
streaming deferred and R3 Wayland injection behind the pending-consent
contract until their measured/platform gates change.
