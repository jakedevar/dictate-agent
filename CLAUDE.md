# CLAUDE.md

Claude-facing orientation for this repo. For the project's architecture,
pipeline diagram, and per-component design notes, read `AGENTS.md` first —
this file does not duplicate that; it only covers what AGENTS.md doesn't:
current repo state, how to build/test, and where things live.

## Current state (as of the S02 daemon-skeleton slice, 2026-08-07)

Two implementations live side by side on purpose:

- **Rust workspace — live, primary.** `Cargo.toml` (workspace root) +
  `crates/*`. Code-complete against the Python daemon's behavior
  (Phases 1–7 of the rewrite) and now driven by a real control plane
  (S02): 379 tests passing, clippy-clean, `cargo build --release`
  producing `dictated` (daemon) and `dictate` (CLI).
- **Python reference daemon — retained, Jake's daily driver.** `dictate/`
  (13 modules). This is **not** legacy cruft to delete — it stays present
  and runnable until the v1.0 parity cutover gate defined in
  `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md`.
  Do not remove or modify it outside of a slice that explicitly says to.

If you find a `CLAUDE.md` or note elsewhere claiming the Rust code lives
in `src_rust_archive/` with Python as primary, it's stale — that was the
inverse of reality on this branch and was reconciled in S00. There is no
`src_rust_archive/` directory.

## Workspace layout

```
crates/
  dictate-proto    wire protocol: commands, events, errors, versioned
                    envelope, binary audio-frame codec. Types + serde only —
                    no transport, no tokio, no I/O. See docs/protocol.md
  dictate-audio    cpal capture (AudioCapture)
  dictate-stt      whisper-rs transcription (Transcriber) + WhisperConfig
  dictate-fmt      grammar correction + text cleanup (GrammarCorrector) + GrammarConfig
  dictate-history  SQLite interaction log (HistoryStore) + HistoryConfig
  dictate-inject   clipboard-paste output (OutputHandler) + OutputConfig
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
`dictate-stt`}, and `dictate-stt` → `dictate-fmt` (for `text_cleanup`).
Nothing depends back on `dictate-core`. This is why each leaf crate owns
its own `XConfig` struct (e.g. `dictate-fmt::GrammarConfig`) instead of
pulling it from `dictate-core::config` — `dictate-core` already depends on
every leaf crate, so the reverse would be a circular crate dependency.
`dictate-core::config::Config` re-exports and aggregates all of them.

`dictate-proto` deliberately depends on nothing in the workspace, and
nothing yet depends on it — S02 (daemon), S32 (UI), and S33 (network API)
are its consumers. It is pinned by golden-JSON tests; read the
compatibility rule in its crate docs before changing any wire type.

Crates NOT yet created (owned by later slices in the master plan, do not
add empty shells for these): `dictate-vad` (S11), `dictate-dict` (S22),
`dictate-context` (S23), `dictate-hotkey` (S31), `dictate-server` (S33).

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

Preferred: use `just` or `make`, both of which set `PATH` for you.

```bash
just build      # cargo build --workspace
just test       # cargo test --workspace   (>= 379 tests must pass)
just clippy     # cargo clippy --all-targets --workspace  (must be clean)
just check      # test + clippy
just release    # cargo build --release --workspace
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
