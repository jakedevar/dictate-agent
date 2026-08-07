---
date: 2026-08-07T22:49:51+00:00
researcher: Claude Sonnet 5
git_commit: bacc0ca742c116055dfe239959713f533910b606
branch: master
repository: dictate_agent
topic: "S00 — Repo reconciliation & workspace scaffold"
tags: [implementation, refactor, cargo-workspace, s00, wisprflow-parity]
status: complete
last_updated: 2026-08-07
last_updated_by: Claude Sonnet 5
type: implementation_strategy
---

# Handoff: S00 — Repo reconciliation & workspace scaffold

## Original Request
S00, the Wave 0 foundation slice of the dictate-agent -> Wispr Flow parity
rebuild (see `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md`).
Three tasks: (1) restore the Python reference daemon (`dictate/`) that a
prior "saving agent created work" commit (`5e16667`) deleted as collateral
damage; (2) convert the single-crate Rust port into the target Cargo
workspace layout (mechanical move); (3) finish the outstanding handoff
leftovers (clippy, release build, deploy artifacts, CLAUDE.md, durable
build-env docs). S01 (protocol crate) and S02 (daemon) are blocked on this
slice landing clean.

## Task(s)
- [x] Task 1 — restore `dictate/` from `origin/master`, verify it compiles
      (`python -m compileall`), standalone commit.
- [x] Task 2 — split the 12-module single crate into 7 workspace crates
      per the master plan's target architecture.
- [x] Task 3 — clippy clean, `cargo build --release`, systemd unit,
      config.example.toml, scripts/Makefile.
- [x] Task 4 (folded into Task 3's scope) — CLAUDE.md + durable CUDA
      build-env docs (justfile).
- [x] Verify the built daemon binary's SIGUSR1/SIGUSR2/SIGTERM signal path
      end-to-end in an isolated `$HOME` sandbox.

## Recent changes
1. `b458657` — `restore: Python reference daemon (dictate/) deleted by 5e16667`
   — `git checkout origin/master -- dictate/`, verbatim restore, no source
   edits, `python -m compileall dictate` passes.
2. `449d77d` — `refactor: split single crate into Cargo workspace (S00)` —
   the workspace scaffold (see Learnings below for the config.rs split
   design decision).
3. `adda375` — `build: adapt Makefile/systemd unit to the crates/dictated rename (S00)`
   — Makefile installs the `dictated` binary under the unchanged name
   `dictate-agent`; systemd `RUST_LOG` updated to the new per-crate
   directive list.
4. `bacc0ca` — `docs: add CLAUDE.md, justfile, fix AGENTS.md links post-split (S00)`
   — new CLAUDE.md, new justfile, and AGENTS.md's 9 stale `file://` links
   into the old `src/` layout repointed at `crates/*/src/`.

## Learnings

### config.rs could not move to dictate-core wholesale
The crate boundaries as specified put `config.rs`'s aggregate types in
`dictate-core`, but `dictate-core` (via `agent.rs`) already depends on
every leaf crate (`dictate-audio`, `dictate-fmt`, `dictate-history`,
`dictate-inject`, `dictate-stt`) for their corrector/handler/store types.
If those same leaf crates also depended on `dictate-core` for their own
`XConfig` struct, that's a circular crate dependency — does not compile.
Resolved by having each leaf crate own its domain config type
(`GrammarConfig` in dictate-fmt, `WhisperConfig` in dictate-stt,
`HistoryConfig` in dictate-history, `OutputConfig` in dictate-inject),
re-exported into `dictate-core::config::Config`'s aggregate. This is a
real design decision, not a pure mechanical move — flagged per the S00
instructions rather than silently invented. No `// TODO(S0x)` stub was
needed anywhere; every module found a compiling home.

The trivial (~8-line) `expand_tilde` path helper is duplicated verbatim
into `dictate-stt` and `dictate-history` (which each call it directly on
their own config's path field) rather than shared, for the same
circular-dependency reason — not worth inventing a crate for one pure
function.

### tracing EnvFilter default had to expand from 1 directive to 7
`main.rs`'s hardcoded fallback was `"dictate_agent=info"` — a single
crate's module-path prefix. Post-split, first-party code spans 7 crate
names (`dictate_core`, `dictate_audio`, `dictate_stt`, `dictate_fmt`,
`dictate_history`, `dictate_inject`, `dictated`), so the default now adds
one `{crate}=info` directive per crate. Verified live: an isolated-HOME
smoke test showed INFO logs from `dictate_core::agent`, `dictate_audio`,
`dictate_stt::transcribe`, `dictate_history::history` all coming through
at the expected level. systemd's `RUST_LOG=dictate_agent=info` was
similarly stale and updated to match.

### Binary rename handled without touching user-facing paths
The architecture diagram names the daemon binary `dictated`. The Makefile
now builds `crates/dictated`'s `dictated` binary but still installs it as
`~/.local/bin/dictate-agent`, so `scripts/run.sh`, `scripts/dictate-toggle`,
`scripts/dictate-cancel`, and the systemd unit's `ExecStart` needed zero
changes. Only the Makefile's `TARGET_BIN` source path changed.

## Artifacts
- `Cargo.toml` (workspace root, new `[workspace]` + `[workspace.dependencies]`)
- `crates/{dictate-audio,dictate-stt,dictate-fmt,dictate-history,dictate-inject,dictate-core,dictated}/`
- `Makefile`, `systemd/dictate-agent.service` (updated)
- `CLAUDE.md` (new), `justfile` (new), `AGENTS.md` (links fixed)
- `dictate/` (restored, 13 files, untouched beyond restoration)
- This handoff.

## Action Items & Next Steps
- S01 (protocol crate, `dictate-proto`) and S02 (daemon skeleton +
  `dictated` UDS control plane) are now unblocked.
- No `// TODO(S0x)` stubs were left in `dictate-core` — every module found
  a non-circular home. Nothing deferred structurally.
- Nothing else outstanding from the original April rewrite handoff was
  found beyond what Task 3 covered (clippy/release/deploy artifacts).

## VERIFICATION_ITEMS:

### AUTOMATED
- `cargo test --workspace` — 78/78 passing (1 dictate-audio + 54
  dictate-core + 13 dictate-fmt + 3 dictate-history + 0 dictate-inject +
  7 dictate-stt), exact match to the pre-split baseline; no test added,
  removed, or modified.
- `cargo clippy --all-targets --workspace` — zero warnings, zero errors.
- `cargo build --release --workspace` — succeeds, produces
  `target/release/dictated` (43.6MB stripped ELF).
- `just test` — verified working end-to-end (validates the justfile's
  PATH-export mechanism against the live workspace).

### DAEMON-level
- [DONE] Built `dictated` binary starts, writes its PID file, and accepts
  SIGUSR1 (toggle-start), SIGUSR2 (cancel), SIGTERM (graceful shutdown +
  PID file removal) exactly as before the split.
  check: ran the release binary directly with an isolated
  `HOME`/`XDG_CONFIG_HOME`, sent `kill -USR1 $PID`, `kill -USR2 $PID`,
  `kill -TERM $PID` in sequence, tailed the log.
  expected (and observed): "Starting recording..." -> "Using input
  device: default" -> "Recording started (16kHz mono f32)" -> "Recording
  cancelled" -> "Shutdown signal received" -> "Shutting down..." -> "PID
  file removed"; process exits; no orphan process left.

### TUI manual
None — this slice has no TUI surface.

## Other Notes
- Never pushed (`origin/master` untouched); this slice's 4 commits are
  local on `master`, which is 11 commits ahead of `origin/master` overall
  (5 pre-existing + 2 from parallel R2/R4 research-spike sessions that
  landed on `master` concurrently, not authored by this session + 4 from
  this slice).
- `dictate/` was restored and not otherwise modified, per the hard
  constraint.
