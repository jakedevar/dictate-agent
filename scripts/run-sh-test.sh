#!/usr/bin/env bash
# scripts/run.sh must not start the June binary while the dictated unit is
# enabled (post-cutover), and must start it otherwise (pre-cutover/rollback).
# Hermetic: temp HOME, stub systemctl and stub dictate-agent; never the real ones.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"; trap 'rm -rf -- "$tmp"' EXIT
mkdir -p "$tmp/home/.local/bin" "$tmp/bin"
printf '#!/bin/sh\necho started >"%s/started"\n' "$tmp" >"$tmp/home/.local/bin/dictate-agent"
chmod +x "$tmp/home/.local/bin/dictate-agent"
check() { # $1 = stub is-enabled exit code, $2 = expect started (yes|no)
    rm -f "$tmp/started"
    printf '#!/bin/sh\n[ "$2" = is-enabled ] && exit %s\nexit 1\n' "$1" >"$tmp/bin/systemctl"
    chmod +x "$tmp/bin/systemctl"
    HOME="$tmp/home" PATH="$tmp/bin:/usr/bin:/bin" bash "$here/run.sh"
    if [ "$2" = yes ]; then [ -f "$tmp/started" ] || { echo "FAIL: June binary not started (is-enabled=$1)"; exit 1; }
    else [ ! -f "$tmp/started" ] || { echo "FAIL: June binary started while dictated is enabled"; exit 1; }; fi
}
check 0 no    # cutover done: dictated enabled -> launcher stays out of the way
check 1 yes   # pre-cutover or after rollback -> June binary starts as before
echo "run.sh guard: OK"
