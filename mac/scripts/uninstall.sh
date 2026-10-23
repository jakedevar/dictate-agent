#!/bin/bash
# On the Mac: remove /Applications/DictateHub.app (and a leftover
# ~/Applications/DictateHub.app from an older install) and the login item.
# Run from the hub with `mac/scripts/remote.sh uninstall [--purge]`.
#
#   scripts/uninstall.sh            # app + login item; keeps settings and token
#   scripts/uninstall.sh --purge    # also the token, settings and logs
#   scripts/uninstall.sh --remove-signing-keychain
#                                   # also the dedicated signing keychain (a
#                                   # later install gets a new certificate, so
#                                   # privacy grants must be given again)
#
# The Microphone and Accessibility entries stay in System Settings → Privacy &
# Security (this script never touches TCC); remove them there with "−".
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib-launchservices.sh

DEST=$INSTALL_DEST
EXE="$DEST/Contents/MacOS/DictateHub"
purge=0
remove_kc=0
for arg in "$@"; do
    case $arg in
    --purge) purge=1 ;;
    --remove-signing-keychain) remove_kc=1 ;;
    *) sed -n '2,14p' "$0" >&2; exit 2 ;;
    esac
done

installed_pids() { pgrep -f "^$EXE" || true; }

if [[ -d $DEST ]]; then
    echo "==> removing the login item"
    out=$(mktemp)
    # Through LaunchServices, so SMAppService runs as the app itself.
    ls_open -W -n -g --stdout "$out" --stderr "$out" "$DEST" --args --unregister-login-item || true
    sed 's/^/    /' "$out"
    rm -f "$out"

    pids=$(installed_pids)
    if [[ -n $pids ]]; then
        echo "==> stopping DictateHub (pid $(echo $pids))"
        kill $pids 2>/dev/null || true
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            [[ -z $(installed_pids) ]] && break
            sleep 0.5
        done
        left=$(installed_pids)
        [[ -z $left ]] || kill -9 $left 2>/dev/null || true
    fi
    echo "==> deleting $DEST"
    ls_unregister "$DEST"
    rm -rf "$DEST"
    # A later install is a first install again: it turns Open at login back on.
    ls_defaults delete dev.jakedevar.DictateHub loginItemRegisteredPath 2>/dev/null || true
    ls_defaults delete dev.jakedevar.DictateHub loginItemConfigured 2>/dev/null || true
else
    echo "==> $DEST is not installed"
fi

if [[ -d $LEGACY_DEST && $LEGACY_DEST != "$DEST" ]]; then
    echo "==> deleting the old copy $LEGACY_DEST"
    pids=$(pgrep -f "^$LEGACY_DEST/Contents/MacOS/DictateHub" || true)
    [[ -z $pids ]] || kill $pids 2>/dev/null || true
    ls_unregister "$LEGACY_DEST"
    rm -rf "$LEGACY_DEST"
fi

if [[ $purge == 1 ]]; then
    echo "==> --purge: token, settings, logs"
    rm -rf "$HOME/Library/Application Support/DictateHub" "$HOME/Library/Logs/DictateHub"
    ls_defaults delete dev.jakedevar.DictateHub 2>/dev/null || true
fi
if [[ $remove_kc == 1 ]]; then
    echo "==> --remove-signing-keychain"
    kc="$HOME/Library/Keychains/dictatehub-signing.keychain-db"
    [[ -e $kc ]] && { security delete-keychain "$kc" 2>/dev/null || rm -f "$kc"; }
    rm -f "$HOME/Library/Keychains/dictatehub-signing.password"
fi
echo "UNINSTALLED"
