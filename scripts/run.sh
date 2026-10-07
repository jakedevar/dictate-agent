#!/bin/bash
# Start the dictation daemon at i3 login (`exec --no-startup-id .../run.sh`).
#
# After scripts/cutover.sh, the systemd user unit `dictated` owns dictation and
# this launcher must not start the old June binary next to it (two daemons, two
# models in VRAM). scripts/cutover-rollback.sh disables the unit, so the next
# login (or a manual run) starts the June binary again. Before any cutover the
# unit is not enabled and this behaves exactly as it always has.
if command -v systemctl >/dev/null 2>&1 &&
    systemctl --user is-enabled --quiet dictated 2>/dev/null; then
    exit 0
fi
exec "$HOME/.local/bin/dictate-agent"
