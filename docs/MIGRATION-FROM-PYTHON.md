# Migrating from the Python / monolithic daemon

**Status: the cutover is complete.** `dictated` (systemd user unit
`dictated.service`) is the live daemon and the i3 bindings call the `dictate`
CLI. The Python reference daemon (`dictate/`) and the scripted cutover
(`scripts/cutover*.sh`, their Python helpers and the `just cutover-test` harness)
were deleted under Issue #3618 (operator directive 2b22862d, "no Python").
They remain in git history: the last commit that contains them is `b85e953`
(`git show b85e953:docs/MIGRATION-FROM-PYTHON.md` has the original runbook and
`git checkout b85e953 -- dictate/` restores the Python daemon if ever needed).

## What is shared and what is separate

| | legacy daemons | `dictated` |
|---|---|---|
| binary | monolithic `dictate-agent` (June 2026 Rust port) | `dictated` + `dictate` |
| unit | `dictate-agent.service` | `dictated.service` |
| PID file | `~/.config/dictate-agent/dictate.pid` | `$XDG_RUNTIME_DIR/dictate-agent/dictated.pid` |
| socket | none | `$XDG_RUNTIME_DIR/dictate-agent/dictated.sock` |
| history | `~/.local/share/dictate-agent/history.db` | `~/.local/share/dictated/history.db` |
| config | `~/.config/dictate-agent/config.toml` — **shared** | same file |
| models | `~/.local/share/dictate-agent/models/` — shared (read) | same directory |

`dictated` claims the legacy PID file only when nothing live holds it, so
`scripts/dictate-toggle` and `scripts/dictate-cancel` drive whichever daemon is
running. `dictated` can still import an old Python-era `history.db` (a file
format, not Python code) via `[history] import_python_db`.

## First install on a machine that still has the old binary

```bash
just install && just install-unit      # side by side; nothing is started
dictated --check-config                # old Python-era keys map over; unknowns warn once
dictate model pull                     # no-op if the model is already verified
systemctl --user enable --now dictated
```

Stop the old daemon yourself first (`pkill -x dictate-agent`), because both
would load a model into VRAM. `scripts/run.sh` (the i3 login launcher) exits
without starting the old binary while `dictated.service` is enabled.

To try `dictated` on a private runtime while another daemon is live, set
`XDG_RUNTIME_DIR`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `DICTATE_SOCKET` to a
temp directory and `[audio] capture = false` (see `scripts/smoke-real.sh`).

## Manual rollback to the retained `dictate-agent` binary

```bash
systemctl --user disable --now dictated
setsid ~/.local/bin/dictate-agent >/dev/null 2>&1 &   # or log out/in: run.sh starts it
```

The old daemon keeps its own history database; `dictated` never writes it. The
shared `config.toml` was never rewritten by the cutover, so no config restore is
needed. Remove the old binary only when you are sure: `make uninstall-legacy`.

## What changed for users

- Config: `[router]` is now `[local]`; HuggingFace model ids map to the pinned
  GGUF catalog. The example config loads with no warnings.
- Control is `dictate <command>` over a socket instead of signals (signals
  still work).
- New: `dictate doctor`, `dictate transcribe`, `dictate history --analytics`,
  dictionary and snippets, the desktop UI.
