---
date: 2026-10-07
author: jakedevar
issue: 1500
slice: cutover-part-1
status: green
---

# Cutover scripts, Issue #1500

Implemented `scripts/cutover.sh` and `scripts/cutover-rollback.sh` on the
assigned RSI branch, based on e3efe8d. The initial implementation checkpoint
is 38959f7; the final commit also contains this handoff, the plain-shell harness
entry point and bounded probe cleanup. No push or branch change was performed.

The scripts reject the account's real HOME without `--live`. Non-live calls
require HOME and all three XDG roots isolated. Every integration case uses a
temporary HOME, a stub systemctl and signalable synthetic daemon processes.
No live cutover, real user-service operation, i3 edit or real data write ran.

Preflight validates the config and runs quick doctor against a private daemon
with capture, hotkeys, history/import and notifications disabled. Its dictionary,
socket and PID files are private. Only named Ollama/model failures, and the
formatter failure accompanying them, may pass preflight. Full postflight doctor
is strict; status/startup/postflight failures automatically return to the old
daemon. All process signals are SIGTERM with verified UID, HOME and identity;
shutdown has a bounded wait and never escalates to SIGKILL.

Timestamped config copies and SQLite online history backups never overwrite
earlier backups; the DB snapshot includes committed WAL rows. Startup uses
generated config copies and a script-owned service drop-in, keeping the shared
config unchanged and redirecting an explicit legacy history path to the new DB.
The import flag applies to the first start only. Future starts use the steady
copy with import disabled. An active-service re-run verifies without restarting,
replacing backups or changing startup flags. Roll back first to change flags.

Rollback disables/stops dictated, removes only the script's own drop-in,
launches `setsid scripts/run.sh` and verifies the legacy PID holder. Config
restoration requires `--restore-config`; it preserves the displaced config
first. The new dictionary/history, backups and logs are retained.

Validation: 23 script tests passed, 0 failed (standalone Python suite and then
the plain-shell entry point). `just check-cpu` ran once in-turn with
`CARGO_BUILD_JOBS=8`, a temporary HOME/XDG roots, disposable Cargo package caches
and a systemctl refusal stub. It passed 1,037 Rust tests across 56 binaries,
0 failed, 0 ignored; all three CPU clippy lines, fmt and the CLI dependency
check passed. Shell syntax and `git diff --check` also passed. Gate log:
`/tmp/cutover-1500-check-cpu.log`; harness log:
`/tmp/cutover-shell-harness.log`. No CUDA gate is required for this script slice.
The shell wrapper was validated separately while the Rust gate continued.

Once the manager's cutover rulings are settled, the operator commands are:

```bash
scripts/cutover.sh --live
scripts/cutover-rollback.sh --live
# Pending choices, each off by default:
scripts/cutover.sh --live --import-history --language en --claude-dictionary
# Optional config restoration:
scripts/cutover-rollback.sh --live --restore-config
```

These live commands are documentation only; the worker never passed `--live`.
The importer still reads `$HOME/.local/share/dictate-agent/history.db`, so use
the default data root with `--import-history`. The existing i3 startup line
still starts the June binary at a later login; that operator decision remains
separate. Existing toggle/cancel scripts work through dictated's legacy PID
claim, or the script prints keybinding lines for manual use.

Touched shared files: `justfile` (harness added to check-cpu and cutover-test)
and `docs/MIGRATION-FROM-PYTHON.md`. No Rust, unit template, legacy run/toggle
script, config fixture or committed vocabulary store changed.

Friction: none
