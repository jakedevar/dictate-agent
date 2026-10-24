#!/usr/bin/env bash
# Offline unit validation only. Never loads units into a running manager.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
if ! command -v systemd-analyze >/dev/null 2>&1; then
    echo 'gpu-gate-units: SKIP (systemd-analyze unavailable)'
    exit 0
fi
tmp="$(mktemp -d /tmp/dgg-units.XXXXXX)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/run" "$tmp/config" "$tmp/units"
# verify checks executable existence; use a known executable in the staged
# copies so validation never depends on installed dictation binaries.
for unit in dictated.service dictate-gpu-status.socket dictate-gpu-status@.service; do
    sed -e 's|^ExecStart=.*|ExecStart=/bin/true|' \
        -e 's|^ExecStartPre=.*|ExecStartPre=/bin/true acquire|' \
        "$repo/systemd/$unit" >"$tmp/units/$unit"
done
XDG_RUNTIME_DIR="$tmp/run" XDG_CONFIG_HOME="$tmp/config" \
    systemd-analyze --user verify --man=no "$tmp/units/dictated.service" \
    "$tmp/units/dictate-gpu-status.socket" "$tmp/units/dictate-gpu-status@.service"
# Assert coordination properties in the originals, too.
rg -qx 'ExecStartPre=%h/.local/bin/dictate-gpu-gate acquire' "$repo/systemd/dictated.service"
rg -qx 'TimeoutStartSec=180' "$repo/systemd/dictated.service"
rg -qx 'RuntimeDirectoryPreserve=yes' "$repo/systemd/dictated.service"
rg -qx 'SocketMode=0600' "$repo/systemd/dictate-gpu-status.socket"
rg -qx 'Accept=yes' "$repo/systemd/dictate-gpu-status.socket"
! rg -n '^(Wants|Requires|BindsTo)=.*dictated' "$repo/systemd/dictate-gpu-status"*
echo 'gpu-gate-units: PASS'
