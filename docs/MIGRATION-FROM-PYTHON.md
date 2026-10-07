# Migrating from the Python / monolithic daemon

The old daemons — the Python reference (`dictate/`) and the monolithic Rust
`dictate-agent` binary — and the new `dictated` are built to coexist until the
v1.0 parity cutover. This guide is how the cutover works once Jake decides to
make it. Installing does not perform it.

## What is shared and what is separate

| | old daemons | `dictated` |
|---|---|---|
| binary | `dictate-agent` / the Python daemon in `dictate/` | `dictated` + `dictate` |
| unit | `dictate-agent.service` | `dictated.service` |
| PID file | `~/.config/dictate-agent/dictate.pid` | `$XDG_RUNTIME_DIR/dictate-agent/dictated.pid` |
| socket | none | `$XDG_RUNTIME_DIR/dictate-agent/dictated.sock` |
| history | `~/.local/share/dictate-agent/history.db` | `~/.local/share/dictated/history.db` |
| config | `~/.config/dictate-agent/config.toml` — **shared** | same file |
| models | `~/.local/share/dictate-agent/models/` — shared (read) | same directory |

`dictated` claims the legacy PID file only when nothing live holds it, so
`scripts/dictate-toggle` and `scripts/dictate-cancel` drive whichever daemon is
running.

## Before cutover

```bash
just install && just install-unit      # side by side; nothing is started
dictated --check-config                # old Python-era keys map over; unknowns warn once
dictate model pull                     # no-op if the model is already verified
```

Test `dictated` on a private runtime first if the old daemon is live: set
`XDG_RUNTIME_DIR`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `DICTATE_SOCKET` to a
temp directory and `[audio] capture = false` (see `scripts/smoke-real.sh`).

## Cutover (operator step)

1. `cp ~/.local/share/dictate-agent/history.db{,.pre-cutover}` and
   `cp ~/.config/dictate-agent/config.toml{,.pre-cutover}`.
2. Set `import_python_db = true` under `[history]` for one start: the Python
   database is read, never modified, and each source is imported once. Set it
   back to `false` after.
3. `systemctl --user disable --now dictate-agent` (or stop the i3-launched
   `scripts/run.sh` binary), then `systemctl --user enable --now dictated`.
4. Point your window-manager bindings at `dictate toggle` / `dictate cancel`
   (or keep `scripts/dictate-toggle`, which now reaches `dictated`).
5. `dictate doctor`, then a real dictation.

## Rollback

1. `systemctl --user disable --now dictated`.
2. `systemctl --user enable --now dictate-agent` (or restart the i3 `exec`).
3. Nothing to restore: the old history database was never written by
   `dictated`, and the config file is only read, except for edits the UI makes
   through `set_config`. If you used the UI settings editor, restore
   `config.toml.pre-cutover`.

Remove the old binary only when you are sure: `make uninstall-legacy`.

## What changed for users

- Config: `[router]` is now `[local]`; HuggingFace model ids map to the pinned
  GGUF catalog. The example config loads with no warnings.
- Control is `dictate <command>` over a socket instead of signals (signals
  still work).
- New: `dictate doctor`, `dictate transcribe`, `dictate history --analytics`,
  dictionary and snippets, the desktop UI.
