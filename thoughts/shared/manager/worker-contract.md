# dictate-agent worker contract (integrator model)

Issued 2026-09-29 by the appointed RSI manager. You are a short-lived worker
launched for exactly one slice. You build it, verify it, commit it on your
sandbox branch, report, and end. The manager integrates, reviews and verifies.

Read, in this order: this file, `CLAUDE.md`,
`thoughts/shared/plans/2026-09-29-wave2-integration-contract.md`, then the
slice section of
`thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md`.

## 1. Start

1. Your sandbox is cut from `master`. First run
   `git merge --ff-only <BASE_SHA>` with the base named in your prompt (it is a
   descendant of `master` on the manager's branch; every sandbox shares one
   object store). Confirm `git log -1` shows it and `git status` is clean.
2. Never `git checkout`, `git switch`, create branches, rebase shared history,
   push, or touch the main checkout at `/home/jakedevar/dictate_agent`.
3. Do the work yourself. Do not launch provider-native subagents, background
   agents, or parallel agents; do not spawn RSI children.

## 2. Build and test environment

- `export PATH="/opt/cuda/bin:$PATH"` (or use `just`). Default builds are
  **CPU-only**: `--workspace` never compiles whisper.cpp's CUDA backend (the
  `cuda` feature is opt-in via `dictated/cuda`; see CLAUDE.md "Build & test").
  The CUDA backend costs about 1000 CPU-seconds / 2.5 minutes wall; do not
  trigger it unless your diff touches `dictate-stt`, the transcription path or
  the CUDA config.
- The host is shared: build with `-j 8` (`CARGO_BUILD_JOBS=8`), one cargo
  build at a time, and check `uptime` before a cold build (wait if the 1-minute
  load is above 40).
- The RSI harness exports a long `TMPDIR`. Unix-socket fixtures must live under
  a short root (see `socket_root()` in `crates/dictated/tests/harness/mod.rs`).
- Run long commands in the foreground with a generous timeout and `tee` them to
  a log under `/tmp`. **Never end your turn to wait for a build, test run or
  notification** — your session ends with your turn and background jobs die.
  If a command times out, rerun it; cargo resumes.
- **Project rule, which overrides the generic RSI catalog advice to use
  `AgentSubmitJob` for long gates:** `AgentSubmitJob` cannot run this repo's
  gates. Its typed cargo params have no clippy, `just` recipe or feature lines.
  Run `just check-cpu` in your own turn as described above. This gap is already
  tracked as #1466 and reported to RSI, so do not file it again.
- Real-hardware checks (CUDA whisper, Ollama, X11) are allowed on this machine
  but must be isolated: a private Xvfb display for anything that types or
  reads windows (never `DISPLAY=:0` for injection), a temp
  `XDG_RUNTIME_DIR`/`XDG_DATA_HOME`/`DICTATE_SOCKET` for any daemon you start,
  and no microphone capture (`pre_roll_ms = 0` / audio-less mode; use WAV
  fixtures). Never signal, stop or reconfigure Jake's running
  `~/.local/bin/dictate-agent`, and never write under
  `~/.config/dictate-agent/` or `~/.local/share/dictate-agent/`.

## 3. Build it right

- The slice's stated intent and acceptance criteria are the ground truth.
  Tests assert that intent, including must-not-change negatives.
- Follow the patterns of the code you touch: honest `StageTiming`s, fail-open
  formatting, deny-by-default capabilities, commit-point cancellation, and the
  additive-only protocol rule.
- The repository is public: synthetic fixtures only; never commit real
  transcripts, history rows, window titles, or personal vocabulary.
- If you find a defect outside your slice, file it with `AgentCreateIssue`
  (title, intent, acceptance criteria) instead of fixing it here.

## 4. Verify (all must pass before you report green)

```bash
just check-cpu 2>&1 | tee /tmp/<slice>-gate.log
```

`just check-cpu` is the worker gate and never compiles the CUDA backend. It runs:

```bash
cargo test --workspace --all-targets                                    # 0 failures
cargo clippy --workspace --all-targets -- -D warnings
# feature-gated targets are invisible to the line above; compile them too
cargo clippy -p dictated --features e2e-real --all-targets -- -D warnings
cargo clippy -p dictate-context --features x11-tests --all-targets -- -D warnings
cargo fmt --all -- --check
cargo tree -p dictate-cli -e normal | rg 'whisper|dictate-fmt'          # must print nothing
```

**CUDA variants are the integrator's job**, once per integration, together
with the real-GPU `just e2e` (WER 0.000): `just check-cuda` (clippy of the
workspace and of `-p dictated --features e2e-real,cuda` with the CUDA backend)
and `just e2e`. A worker runs them only when its diff touches `dictate-stt`,
the transcription path, a `cuda` feature or `.cargo/config.toml`'s CUDA
settings — and then says so in its RESULT line.

Report the exact passed/failed counts you saw. Never report a run you did not
see finish.

## 5. Commit and report

- Conventional commit subjects (`feat(dict): …`, `fix(core): …`,
  `test(…)`, `docs(…)`), explicit paths (never `git add -A`), one logical
  change per commit. Commit a checkpoint after each completed step so work
  survives an interrupted session.
- Write your design notes and measured numbers to
  `thoughts/shared/handoffs/general/<date>_<slice>.md` and commit them.
- Your final message starts with exactly one line:
  `RESULT <full 40-char sha> slice=<id> tests="<passed>/<failed>" status=<green|red: reason|partial: reason>`
  then at most 12 lines: what changed, decisions the integrator must know,
  conflict risks (files outside your ownership you touched), follow-up Issues
  filed, and anything that needs Jake (a genuine product decision).
- If your context grows past ~60% of its window, commit, write the handoff, and
  report `status=partial` with precise next steps.

## 6. Never

- Push, force-push, change branches, or edit `master`.
- Weaken a test to make it pass, or mark a real failure `#[ignore]`.
- Put credentials or `$RSI_SESSION_TOKEN` anywhere.
- Delete user data, or touch Jake's live daemon, config, or databases.
