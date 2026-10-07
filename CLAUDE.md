# CLAUDE.md

Claude-facing orientation for this repo. For the project's architecture,
pipeline diagram, and per-component design notes, read `AGENTS.md` first —
this file does not duplicate that; it only covers what AGENTS.md doesn't:
current repo state, how to build/test, and where things live.

## Current state (as of Wave 1 integration + manager base, 2026-09-29)

- **Rust workspace — live, primary.** `Cargo.toml` (workspace root) +
  `crates/*`. Waves 0–1 of the Wispr Flow parity plan are integrated: protocol,
  control plane, audio v2, Silero VAD, STT v2 with model manager, injection
  v2, history v2, evdev hotkeys. 419+ tests pass, `clippy -D warnings` and
  `cargo fmt --check` are clean. `cargo build --release` produces `dictated`
  (daemon) and `dictate` (CLI).
- **Jake's daily driver is `~/.local/bin/dictate-agent`** — a monolithic Rust
  binary built 2026-06-18 from the pre-workspace port, launched by i3 through
  `scripts/run.sh`. It shares `~/.config/dictate-agent/config.toml` (still in
  the Python-era shape) with `dictated`, and writes
  `~/.local/share/dictate-agent/history.db`. Never stop, signal, or
  reconfigure it outside the cutover slice.
- **Python reference daemon — retained, not running.** `dictate/` (13
  modules) stays present and runnable until the v1.0 parity cutover gate
  defined in
  `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md`.
  Do not remove or modify it outside of a slice that explicitly says to.
- **The GitHub repo is public.** Never commit real transcripts, history rows,
  window titles, or personal vocabulary; fixtures are synthetic.
- Active plan for Wave 2+:
  `thoughts/shared/plans/2026-09-29-wave2-integration-contract.md`; worker
  rules: `thoughts/shared/manager/worker-contract.md`.

There is no `src_rust_archive/` directory; notes claiming otherwise are stale.

## Workspace layout

```
crates/
  dictate-proto    wire protocol: commands, events, errors, versioned
                    envelope, binary audio-frame codec. Types + serde only —
                    no transport, no tokio, no I/O. See docs/protocol.md
  dictate-audio    cpal capture with idle pre-roll ring, AGC, device recovery,
                    earcons (AudioCapture) + AudioConfig
  dictate-vad      Silero VAD gate/trim + hands-free trailing-silence stop
  dictate-stt      SttProvider + whisper-rs impl, pinned model catalog/pull,
                    language/initial_prompt hooks + WhisperConfig
  dictate-fmt      deterministic text chain (S20), LLM formatting pass under
                    [format.llm] (S21, LlmFormatter) + FormatConfig
  dictate-history  SQLite WAL + FTS5 interaction log, analytics, retention,
                    privacy mode, Python-DB import (HistoryStore) + HistoryConfig
  dictate-inject   Injector boundary: X11 paste (clipboard save/restore) and
                    direct typing, Wayland portal stub + OutputConfig
  dictate-hotkey   passive evdev hold-to-talk / toggle / cancel service
  dictate-core     the engine: session state machine (session.rs), cancellable
                    pipeline (pipeline.rs), the command-serializing actor
                    (engine.rs), cancellation with a commit point (cancel.rs),
                    event fan-out (event_bus.rs), and the hardware seams with
                    their test doubles (ports.rs). Also router, timer, local
                    LLM executor, notifications, aggregate Config
  dictated         daemon: UDS JSON-RPC server (server.rs), SIGUSR1/2 shim
                    (signals.rs), runtime paths (paths.rs). lib + bin, so
                    tests/control_plane.rs drives a real daemon
  dictate-cli      the `dictate` control client. Depends on dictate-proto and
                    nothing else — a keybinding runs it on every dictation,
                    so it must not link whisper/CUDA (1.4MB vs the daemon's 45MB)
dictate/           Python reference daemon (retained until cutover)
```

Crate dependency direction is one-way: `dictated` → `dictate-core` →
{`dictate-audio`, `dictate-fmt`, `dictate-history`, `dictate-inject`,
`dictate-stt`}; `dictate-fmt` → `dictate-proto` (for `FormatContext`).
`dictate-stt` no longer depends on `dictate-fmt`: it returns raw Whisper
text, and corrections/cleanup run in the S20 text chain.
Nothing depends back on `dictate-core`. This is why each leaf crate owns
its own `XConfig` struct (e.g. `dictate-fmt::GrammarConfig`) instead of
pulling it from `dictate-core::config` — `dictate-core` already depends on
every leaf crate, so the reverse would be a circular crate dependency.
`dictate-core::config::Config` re-exports and aggregates all of them.

`dictate-proto` deliberately depends on nothing in the workspace; the daemon,
CLI, history and injection crates consume it today, and S32 (UI) and S33
(network API) will. It is pinned by golden-JSON tests; read the
compatibility rule in its crate docs before changing any wire type.

Crates NOT yet created (owned by later slices in the master plan, do not
add empty shells for these): `dictate-dict` (S22), `dictate-context` (S23),
`dictate-server` (S33). `dictate-vad` (S11) and `dictate-hotkey` (S31) exist.

`dictate-core` gained a dependency on `dictate-proto` in S02 (the engine
speaks the wire types directly). The direction is still one-way — nothing
depends back on `dictate-core`, and `dictate-proto` still depends on
nothing.

## Build & test

whisper-rs builds whisper.cpp via CMake, which has two build traps on this
machine (Arch Linux, CUDA at `/opt/cuda`, RTX 5080):

- `WHISPER_DONT_GENERATE_BINDINGS=1` — avoids a bindgen struct-size
  mismatch. Already set workspace-wide in `.cargo/config.toml`, along with
  `CUDACXX`/`CUDAHOSTCXX` pinned to a compatible GCC.
- `PATH` must include `/opt/cuda/bin` (nvcc) — this is the one env var
  that has to come from the invoking shell/tool, since Cargo can't inject
  into its own subprocess's PATH search from `.cargo/config.toml`.

### CUDA is opt-in (build-cost rule)

whisper.cpp's CUDA backend (cicc/ptxas) is about 1000 CPU-seconds / 2.5 minutes
wall per cold target dir, and every sandbox has its own. So **default builds are
CPU-only**: `dictate-stt`'s default is `transcribe` (not `cuda`), and the CUDA
backend is enabled only through `dictated/cuda` → `dictate-core/cuda` →
`dictate-stt/cuda`. `cargo build|test|clippy --workspace` never compiles it;
`just release`, `just install`, `make release|install` and `just e2e` pass
`dictated/cuda`, so Jake's installed `dictated` still has CUDA. A plain
`cargo build --release` yields a CPU-only `dictated` (`dictate status` reports
`backend: cpu`) — use `just release`.

- `.cargo/config.toml` pins `CMAKE_CUDA_ARCHITECTURES = "120a-real"` (RTX 5080,
  sm_120): one arch instead of ggml's 75..121 list. Verified: the CMake cache
  holds `120a-real` and the only `--generate-code` is `compute_120a/sm_120a`.
  (On this host with a GPU visible ggml's default is `native`, which resolves to
  the same arch; the pin matters when no GPU is visible.) For a multi-arch
  release export the variable — a shell value wins over `[env]`:
  `CMAKE_CUDA_ARCHITECTURES="75-virtual;80-virtual;86-real;89-real;120a-real" just release`.
- **Workers** gate with `just check-cpu` (tests, clippy incl. the `e2e-real` and
  `x11-tests` feature lines, fmt, dictate-cli tree check). Only a diff touching
  `dictate-stt`, the transcription path, a `cuda` feature or the CUDA config also
  runs `just check-cuda`.
- **The integrator** runs `just check-cuda` (CUDA clippy variants) and the
  real-GPU `just e2e` once per integration.
- sccache was measured and is **not** enabled: a second target dir at a different
  path reused ~0% of the CUDA compiles (see
  `thoughts/shared/handoffs/general/2026-10-06_build-cuda-opt-in.md`).
- Host etiquette: `-j 8`, one cargo build at a time, check `uptime` before a cold
  build.

Preferred: use `just` or `make`, both of which set `PATH` for you.

```bash
just build      # cargo build --workspace            (CPU-only)
just test       # cargo test --workspace             (CPU-only)
just clippy     # cargo clippy --all-targets --workspace  (CPU-only, must be clean)
just check-cpu  # WORKER gate: test + clippy (+ feature lines) + fmt + cli tree
just check-cuda # INTEGRATOR gate: clippy with the CUDA backend
just check      # test + clippy
just release    # cargo build --release --workspace --features dictated/cuda
just install    # release + install `dictated` and `dictate` to ~/.local/bin
just install-unit  # install systemd/dictated.service

# or, without just:
make release
make test
make clippy
make install
make install-unit
```

If you're running `cargo` directly outside `just`/`make`, export PATH
yourself first:

```bash
export PATH="/opt/cuda/bin:$PATH"
cargo test --workspace
```

## Runtime control (S02)

Two surfaces, one code path underneath — the signal shim calls the same
engine handlers the protocol commands do (`crates/dictated/src/signals.rs`).

```bash
dictate status          # daemon + session state, capabilities, model backend
dictate toggle          # start, or stop-and-transcribe if recording
dictate cancel          # abandon the session; injects nothing
dictate tail            # stream events (state_changed, final, error, level)
dictate history         # past dictations from the interaction log
```

- Socket: `$XDG_RUNTIME_DIR/dictate-agent/dictated.sock`
  (override with `DICTATE_SOCKET`).
- PID file: `$XDG_RUNTIME_DIR/dictate-agent/dictated.pid`.
- History DB: `$XDG_DATA_HOME/dictated/history.db`.
- systemd user unit: `systemd/dictated.service` (`make install-unit`).

`scripts/dictate-toggle` / `scripts/dictate-cancel` are **unchanged** and
still work: they signal whatever holds the legacy PID file at
`${XDG_CONFIG_HOME:-$HOME/.config}/dictate-agent/dictate.pid`, and
`dictated` claims that file **only when nothing live already holds it**
(`crates/dictated/src/paths.rs`). So with the Python daemon running the
scripts drive Python; with only `dictated` running they drive `dictated`.
Neither ever overwrites a PID file a living process owns — which is the
whole reason the two can stay installed side by side.

### Headless transcription, doctor, real-hardware checks (S03)

```bash
dictate transcribe clip.wav [--json] [--inject] [--route R] [--privacy]
                        # upload a WAV; text on stdout, summary on stderr.
                        # Never types unless --inject (and the connection may).
dictate doctor [--quick] [--json]
                        # one named check per dependency, each with a one-line
                        # fix; exits 1 on a failure. Works with no daemon.
dictated --check-config [--config PATH]
                        # effective config + every warning; exit 1 if invalid
just e2e                # (integrator, once per integration) real CUDA Whisper + Silero VAD, in-process daemon,
                        # synthetic fixtures, WER + per-stage p50/p95 (GPU + model)
just smoke              # release binaries, isolated dictated, doctor + transcribe
```

- `transcribe_audio` (inline WAV/PCM) runs as an *upload session* in the
  engine's single slot: no microphone, media pause, earcons or `recording`
  state (`SessionHandle::is_upload`). `[audio] capture = false` starts the
  daemon with no input device at all (`DisabledAudioSource`); `pre_roll_ms = 0`
  closes the device while idle (`dictate status` → `mic`).
- Config loading maps Python-era shapes (`[router]`→`[local]`, HF model ids)
  and **warns once per unknown section/key** — the example config loads clean
  (tested). `formatter.health` in `get_status` exposes a fail-open formatter.
- The `e2e-real` cargo feature gates `crates/dictated/tests/e2e_real.rs`; the
  default `cargo test --workspace --all-targets` needs no GPU, model, network
  or display. Real-hardware checks run against a private temp runtime and never
  touch the live daemon (see `scripts/smoke-real.sh` for the isolation rules).

### Paths must never collide with the Python daemon

The Python reference daemon owns `~/.config/dictate-agent/dictate.pid` and
`~/.local/share/dictate-agent/history.db`. `dictated` deliberately uses
different paths for all of socket, PID, and DB. If you add runtime state,
keep it out of the Python daemon's namespace until the v1.0 cutover gate.

## Where to look next

- `AGENTS.md` — architecture, pipeline diagram, per-component design.
- `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md` —
  the full Wispr Flow parity rebuild plan (ground truth, target
  architecture, locked decisions, slice-by-slice scope).
- `thoughts/shared/handoffs/general/2026-04-10_17-51-32_rust-rewrite-implementation.md`
  — the original Phase 1–7 Rust rewrite handoff, whisper-rs/ollama-rs/cpal
  API-delta notes.
