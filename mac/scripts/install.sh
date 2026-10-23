#!/bin/bash
# On the Mac: install the bundled app to /Applications/DictateHub.app (owned by
# the console user, no sudo: an admin user can write /Applications) and launch
# it once. Its first launch turns on "Open at login" (SMAppService.mainApp;
# Settings → Startup switches it off). Run from the hub with
# `mac/scripts/remote.sh install`, which bundles first.
#
#   scripts/install.sh [--no-launch] [--allow-adhoc]
#
# Migration: a copy installed by an earlier version at ~/Applications is
# stopped, its login item removed, then it is unregistered from LaunchServices
# and deleted. Privacy grants (Accessibility, Input Monitoring, Microphone) are
# keyed to the designated requirement, which is the same at the new path, so
# they carry over; the stale-grant hint covers a grant pinned to an older build.
#
# An ad-hoc-signed bundle is refused unless --allow-adhoc: its designated
# requirement changes on every build, so the Microphone and Accessibility
# grants would be lost at the next install.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib-signing.sh
source scripts/lib-launchservices.sh

SRC=build/DictateHub.app
DEST_DIR=$INSTALL_DIR
DEST=$INSTALL_DEST
EXE="$DEST/Contents/MacOS/DictateHub"
LEGACY_EXE="$LEGACY_DEST/Contents/MacOS/DictateHub"
LOG_DIR="$HOME/Library/Logs/DictateHub"

launch=1
allow_adhoc=0
for arg in "$@"; do
    case $arg in
    --no-launch) launch=0 ;;
    --allow-adhoc) allow_adhoc=1 ;;
    *) sed -n '2,18p' "$0" >&2; exit 2 ;;
    esac
done

[[ -d $SRC ]] || { echo "no $SRC; run scripts/bundle.sh first" >&2; exit 1; }
codesign --verify --strict "$SRC"
dr=$(codesign -d -r- "$SRC" 2>&1 | grep designated || true)
if ! designated_is_stable "$dr"; then
    if [[ $allow_adhoc == 1 ]]; then
        echo "warning: installing an ad-hoc bundle; privacy grants will not survive the next install" >&2
    else
        echo "error: $SRC is ad-hoc signed ($dr)." >&2
        echo "       Run scripts/signing-setup.sh, rebundle, or pass --allow-adhoc." >&2
        exit 1
    fi
fi

[[ -d $DEST_DIR && -w $DEST_DIR ]] || {
    echo "error: $DEST_DIR is not writable by $(id -un); an admin user can write /Applications without sudo." >&2
    exit 1
}

pids_of() { pgrep -f "^$1" || true; }
stop_pids() { # stop_pids PIDS...
    local pids=$* left
    [[ -n $pids ]] || return 0
    echo "==> stopping the running copy (pid $pids)"
    kill $pids 2>/dev/null || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        left=""
        for p in $pids; do kill -0 "$p" 2>/dev/null && left="$left $p"; done
        [[ -z $left ]] && return 0
        sleep 0.5
    done
    echo "    still running after 5 s; SIGKILL$left"
    kill -9 $left 2>/dev/null || true
}

# --- migrate away from ~/Applications (and any stale copy at the new path)
if [[ -d $LEGACY_DEST && $LEGACY_DEST != "$DEST" ]]; then
    echo "==> migrating: removing the old copy at $LEGACY_DEST"
    # The old copy removes its own login item (through LaunchServices, so
    # SMAppService runs as the app itself). If it was on, the new copy is a
    # first install again and turns it back on; if the user had it off, that
    # stays off.
    out=$(mktemp)
    ls_open -W -n -g --stdout "$out" --stderr "$out" "$LEGACY_DEST" --args --unregister-login-item || true
    sed 's/^/    /' "$out"
    if grep -q 'login item: enabled ->' "$out"; then
        ls_defaults delete "$BUNDLE_ID" loginItemConfigured 2>/dev/null || true
        ls_defaults delete "$BUNDLE_ID" loginItemRegisteredPath 2>/dev/null || true
        echo "    login item was on: the new copy registers it again"
    fi
    rm -f "$out"
    stop_pids $(pids_of "$LEGACY_EXE")
    ls_unregister "$LEGACY_DEST"
    rm -rf "$LEGACY_DEST"
    echo "    removed $LEGACY_DEST"
fi
stop_pids $(pids_of "$EXE")

echo "==> installing $DEST"
staging="$DEST_DIR/.DictateHub.app.installing"
rm -rf "$staging"
ditto "$SRC" "$staging"
# A bundle that was never downloaded carries no quarantine flag; make sure none
# is set (a quarantined, self-signed app is blocked by Gatekeeper).
xattr -dr com.apple.quarantine "$staging" 2>/dev/null || true
ls_unregister "$DEST"
rm -rf "$DEST"
mv "$staging" "$DEST"
codesign --verify --strict "$DEST"
[[ -O $DEST ]] || { echo "error: $DEST is not owned by $(id -un)" >&2; exit 1; }
echo "    $(codesign -d -r- "$DEST" 2>&1 | grep designated)"
echo "    version $(defaults read "$DEST/Contents/Info" CFBundleShortVersionString) ($(defaults read "$DEST/Contents/Info" CFBundleVersion))"
ls_register "$DEST"
echo "    registered with LaunchServices"

if [[ $launch == 0 ]]; then
    echo "INSTALLED=$DEST (not launched)"
    exit 0
fi

mkdir -p "$LOG_DIR"
log="$LOG_DIR/launch.log"
: > "$log"
echo "==> launching (stderr → $log)"
ls_open --stdout "$log" --stderr "$log" "$DEST"
for _ in $(seq 1 20); do
    grep -q 'login item:' "$log" 2>/dev/null && break
    sleep 0.5
done
sed -n '1,20p' "$log"
pids=$(pids_of "$EXE")
[[ -n $pids ]] || { echo "error: DictateHub is not running after launch" >&2; exit 1; }
grep -q 'started: status item installed=true' "$log" || { echo "error: no status item" >&2; exit 1; }
echo "RUNNING pid $(echo $pids)"
echo "INSTALLED=$DEST"
