#!/bin/bash
# Build DictateHub.app on the Mac and code-sign it.
#
#   mac/scripts/bundle.sh            # release build -> mac/build/DictateHub.app
#   CONFIG=debug mac/scripts/bundle.sh
#
# Signing: the first "Apple Development" identity in the keychain (override
# with DICTATEHUB_SIGN_IDENTITY=<SHA-1 or name>), so the designated
# requirement is stable and TCC grants (Microphone, Accessibility) survive
# rebuilds. If that fails (typically: the login keychain is locked in an SSH
# session) the bundle is signed ad hoc instead, and the unlock step is printed.
# DICTATEHUB_ADHOC=1 skips the identity attempt.
set -euo pipefail

cd "$(dirname "$0")/.."
CONFIG=${CONFIG:-release}
APP=build/DictateHub.app
BUNDLE_ID=dev.jakedevar.DictateHub
VERSION=$(tr -d '[:space:]' < VERSION)
BUILD=$(date -u +%Y%m%d%H%M)

# Run a command with a time limit (macOS has no timeout(1)); a keychain
# access prompt on the Mac's screen must not hang an SSH session forever.
with_timeout() { local secs=$1; shift; perl -e 'alarm shift; exec @ARGV' "$secs" "$@"; }

echo "==> swift build -c $CONFIG (arm64)"
swift build -c "$CONFIG" --arch arm64 --product DictateHub
BIN="$(swift build -c "$CONFIG" --arch arm64 --show-bin-path)/DictateHub"

echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/DictateHub"
sed -e "s/__VERSION__/$VERSION/" -e "s/__BUILD__/$BUILD/" Resources/Info.plist > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

identity=${DICTATEHUB_SIGN_IDENTITY:-}
if [[ -z $identity && -z ${DICTATEHUB_ADHOC:-} ]]; then
    # The SHA-1 column, not the name (the name embeds the account's email).
    identity=$(security find-identity -v -p codesigning 2>/dev/null \
        | awk '/"Apple Development/ { print $2; exit }')
fi

signed=adhoc
if [[ -n $identity && -z ${DICTATEHUB_ADHOC:-} ]]; then
    echo "==> codesign with Apple Development identity ${identity:0:8}…"
    if err=$(with_timeout 60 codesign --force --sign "$identity" --identifier "$BUNDLE_ID" \
            --timestamp=none "$APP" 2>&1); then
        signed=identity
    else
        echo "codesign with the identity failed: $err" >&2
        echo "    The login keychain is probably locked in this SSH session. To sign with" >&2
        echo "    the identity over SSH, unlock it first (prompts for the login password):" >&2
        echo "      security unlock-keychain ~/Library/Keychains/login.keychain-db" >&2
        echo "    and, once, allow codesign to use the key without a GUI prompt:" >&2
        echo "      security set-key-partition-list -S apple-tool:,apple: -s ~/Library/Keychains/login.keychain-db" >&2
        echo "    Falling back to ad-hoc signing (TCC grants will NOT survive a rebuild)." >&2
    fi
fi
if [[ $signed == adhoc ]]; then
    echo "==> codesign ad hoc"
    codesign --force --sign - --identifier "$BUNDLE_ID" "$APP"
fi

codesign --verify --strict --verbose=1 "$APP"
echo "==> signature ($signed):"
# Strip anything after "Authority=Apple Development:" (the account's email).
codesign -dvv "$APP" 2>&1 \
    | grep -E '^(Identifier|Format|Signature|TeamIdentifier|Authority)=' \
    | sed -E 's/^(Authority=Apple Development).*/\1: <redacted>/'
codesign -d -r- "$APP" 2>&1 | grep -E 'designated' \
    | sed -E 's/"Apple Development:[^"]*"/"Apple Development: <redacted>"/' || true
echo "SIGNED=$signed"
echo "APP=$PWD/$APP"
