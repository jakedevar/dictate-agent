# CLAUDE.md

Claude-facing orientation for this repo. For the project's architecture,
pipeline diagram, and per-component design notes, read `AGENTS.md` first —
this file does not duplicate that; it only covers what AGENTS.md doesn't:
current repo state, how to build/test, and where things live.

## Current state (as of the S00 workspace-scaffold slice, 2026-08-07)

Two implementations live side by side on purpose:

- **Rust workspace — live, primary.** `Cargo.toml` (workspace root) +
  `crates/*`. Code-complete against the Python daemon's behavior
  (Phases 1–7 of the rewrite), 78 tests passing, clippy-clean,
  `cargo build --release` producing a working binary.
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
  dictate-audio    cpal capture (AudioCapture)
  dictate-stt      whisper-rs transcription (Transcriber) + WhisperConfig
  dictate-fmt      grammar correction + text cleanup (GrammarCorrector) + GrammarConfig
  dictate-history  SQLite interaction log (HistoryStore) + HistoryConfig
  dictate-inject   clipboard-paste output (OutputHandler) + OutputConfig
  dictate-core     agent orchestration, router, timer, local LLM executor,
                    notifications, and the aggregate Config (+ LocalConfig,
                    NotificationConfig, TimerConfig)
  dictated         daemon binary (crates/dictated/src/main.rs) — installed
                    as `dictate-agent` (see Makefile) to keep scripts/ and
                    the systemd unit unchanged
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

Crates NOT yet created (owned by later slices in the master plan, do not
add empty shells for these): `dictate-proto` (S01), `dictate-vad` (S11),
`dictate-dict` (S22), `dictate-context` (S23), `dictate-hotkey` (S31),
`dictate-cli` (S02), `dictate-server` (S33).

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
just test       # cargo test --workspace   (>= 78 tests must pass)
just clippy     # cargo clippy --all-targets --workspace  (must be clean)
just release    # cargo build --release --workspace
just install    # release + install to ~/.local/bin/dictate-agent

# or, without just:
make release
make test
make clippy
make install
```

If you're running `cargo` directly outside `just`/`make`, export PATH
yourself first:

```bash
export PATH="/opt/cuda/bin:$PATH"
cargo test --workspace
```

## Runtime control (unchanged by the S00 split)

The daemon is still controlled by POSIX signals, not a control-plane RPC
(that's S02's job — see the master slice map):

- `scripts/dictate-toggle` sends `SIGUSR1` to toggle recording.
- `scripts/dictate-cancel` sends `SIGUSR2` to cancel and discard.
- PID file: `${XDG_CONFIG_HOME:-$HOME/.config}/dictate-agent/dictate.pid`.
- systemd user unit: `systemd/dictate-agent.service`.

## Where to look next

- `AGENTS.md` — architecture, pipeline diagram, per-component design.
- `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md` —
  the full Wispr Flow parity rebuild plan (ground truth, target
  architecture, locked decisions, slice-by-slice scope).
- `thoughts/shared/handoffs/general/2026-04-10_17-51-32_rust-rewrite-implementation.md`
  — the original Phase 1–7 Rust rewrite handoff, whisper-rs/ollama-rs/cpal
  API-delta notes.
