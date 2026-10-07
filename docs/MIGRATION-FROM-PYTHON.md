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

After the cutover rulings are settled, from this checkout:

```bash
scripts/cutover.sh --live
```

The script requires the installed `~/.local/bin/dictated`, `dictate`, retained
`dictate-agent`, and the unit from `just install-unit`. It also needs Bash,
Python 3.11+, `flock`, `pgrep`, `setsid` and `systemctl`. It refuses the account's
real HOME unless the operator passes `--live`. Without that flag it requires a
temporary HOME and all three XDG roots inside it.

It validates the config, then starts a private preflight daemon with microphone
capture, hotkeys, history writes/import and notifications disabled. Its socket,
PID files and dictionary are private; it checks the configured speech model and
injection backend without recording or typing. `dictate doctor --quick` must
pass, except for the named Ollama/formatter checks. The old daemon keeps running
during preflight. The installed daemon's full `dictate doctor` must pass after
the switch, including those checks.

Before stopping the old daemon, it saves timestamped, private, never-overwritten
copies under `$XDG_CONFIG_HOME/dictate-agent/cutover/` (default
`~/.config/dictate-agent/cutover/`). The history backup uses SQLite's online
backup API so committed WAL rows are included. Missing source files are reported
by their absence in that backup directory; they are not created. It verifies the
old process identity and HOME, sends SIGTERM, waits for shutdown, and enables
`dictated`. A timeout never escalates to SIGKILL. Failed startup, status or full
doctor triggers automatic rollback and a nonzero exit.

Startup uses generated config copies and a script-owned systemd drop-in. The
shared `config.toml`, old database and i3 config stay unchanged. An explicit
legacy history DB path is redirected to the separate `dictated/history.db`.
Re-running the cutover verifies the existing service without restarting it or
replacing the first backups. Startup flags on a re-run are unchanged; roll back
first to change them.

These pending choices are **off unless explicitly passed**:

```bash
scripts/cutover.sh --live --import-history --language en --claude-dictionary
```

`--import-history` enables the existing read-only Python history importer for
the first service start only. The script then points future starts to a copy
with import disabled, without editing the shared config or restarting. The
importer reads `$HOME/.local/share/dictate-agent/history.db`; use the default data
root for this option. `--language en` pins English in the generated service
config until rollback. `--claude-dictionary` adds the local entry through
`dictate dict` only if it is absent. Dictionary contents and diagnostic reports
stay local; never commit the cutover state directory.

If `dictated` holds the legacy PID file, existing `scripts/dictate-toggle` and
`scripts/dictate-cancel` bindings already work. Otherwise the script prints exact
`bindsym` lines for manual use. It never edits or reloads i3. The existing i3
startup `scripts/run.sh` still launches the June binary on a later login; decide
that startup change separately before the next login.

## Rollback

```bash
scripts/cutover-rollback.sh --live
```

This disables and stops `dictated`, removes only the script-owned drop-in,
reloads the unit, and launches the June binary through `setsid scripts/run.sh`.
It verifies that the old binary holds the legacy PID file. Logs and backups,
the new history and the local dictionary remain available. A repeated rollback
verifies the already-running old daemon without launching a duplicate.

Config restoration is opt-in:

```bash
scripts/cutover-rollback.sh --live --restore-config
```

Only when it differs, this restores the newest timestamped pre-cutover config,
saving the displaced config as another private copy first. No history restore
is needed: cutover never makes the new daemon write the old database.

The synthetic integration harness is `just cutover-test` and also runs in
`just check-cpu` / CI. It creates its own temporary HOME, XDG roots, stub
`systemctl` and stub daemon processes; it never passes `--live`.

Remove the old binary only when you are sure: `make uninstall-legacy`.

## What changed for users

- Config: `[router]` is now `[local]`; HuggingFace model ids map to the pinned
  GGUF catalog. The example config loads with no warnings.
- Control is `dictate <command>` over a socket instead of signals (signals
  still work).
- New: `dictate doctor`, `dictate transcribe`, `dictate history --analytics`,
  dictionary and snippets, the desktop UI.
