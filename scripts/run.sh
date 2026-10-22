#!/bin/bash
# Start the dictation daemon at i3 login (`exec --no-startup-id .../run.sh`).
#
# While the systemd user unit `dictated` is enabled it owns dictation and this
# launcher must not start the old June binary next to it (two daemons, two
# models in VRAM). After `systemctl --user disable dictated` (the manual
# rollback in docs/MIGRATION-FROM-PYTHON.md) the next login starts the June
# binary again.
if command -v systemctl >/dev/null 2>&1 &&
    systemctl --user is-enabled --quiet dictated 2>/dev/null; then
    exit 0
fi
exec "$HOME/.local/bin/dictate-agent"
