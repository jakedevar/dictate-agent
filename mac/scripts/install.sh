#!/bin/bash
# On the Mac: install the bundled app to ~/Applications/DictateHub.app (no
# sudo) and launch it once. Its first launch turns on "Open at login"
# (SMAppService.mainApp; Settings → Startup switches it off). Run from the hub
# with `mac/scripts/remote.sh install`, which bundles first.
#
#   scripts/install.sh [--no-launch] [--allow-adhoc]
#
# An ad-hoc-signed bundle is refused unless --allow-adhoc: its designated
# requirement changes on every build, so the Microphone and Accessibility
# grants would be lost at the next install.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib-signing.sh

SRC=build/DictateHub.app
DEST_DIR="$HOME/Applications"
DEST="$DEST_DIR/DictateHub.app"
EXE="$DEST/Contents/MacOS/DictateHub"
LOG_DIR="$HOME/Library/Logs/DictateHub"

launch=1
allow_adhoc=0
for arg in "$@"; do
    case $arg in
    --no-launch) launch=0 ;;
    --allow-adhoc) allow_adhoc=1 ;;
    *) sed -n '2,12p' "$0" >&2; exit 2 ;;
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

installed_pids() { pgrep -f "^$EXE" || true; }

pids=$(installed_pids)
if [[ -n $pids ]]; then
    echo "==> stopping the running copy (pid $(echo $pids))"
    kill $pids 2>/dev/null || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        [[ -z $(installed_pids) ]] && break
        sleep 0.5
    done
    left=$(installed_pids)
    [[ -z $left ]] || { echo "    still running after 5 s; SIGKILL $left"; kill -9 $left 2>/dev/null || true; }
fi

echo "==> installing $DEST"
mkdir -p "$DEST_DIR"
staging="$DEST_DIR/.DictateHub.app.installing"
rm -rf "$staging"
ditto "$SRC" "$staging"
rm -rf "$DEST"
mv "$staging" "$DEST"
codesign --verify --strict "$DEST"
echo "    $(codesign -d -r- "$DEST" 2>&1 | grep designated)"
echo "    version $(defaults read "$DEST/Contents/Info" CFBundleShortVersionString) ($(defaults read "$DEST/Contents/Info" CFBundleVersion))"

if [[ $launch == 0 ]]; then
    echo "INSTALLED=$DEST (not launched)"
    exit 0
fi

mkdir -p "$LOG_DIR"
log="$LOG_DIR/launch.log"
: > "$log"
echo "==> launching (stderr → $log)"
open --stdout "$log" --stderr "$log" "$DEST"
for _ in $(seq 1 20); do
    grep -q 'login item:' "$log" 2>/dev/null && break
    sleep 0.5
done
sed -n '1,20p' "$log"
pids=$(installed_pids)
[[ -n $pids ]] || { echo "error: DictateHub is not running after launch" >&2; exit 1; }
grep -q 'started: status item installed=true' "$log" || { echo "error: no status item" >&2; exit 1; }
echo "RUNNING pid $(echo $pids)"
echo "INSTALLED=$DEST"
