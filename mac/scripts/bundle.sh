#!/bin/bash
# Build DictateHub.app on the Mac and code-sign it.
#
#   mac/scripts/bundle.sh            # release build -> mac/build/DictateHub.app
#   CONFIG=debug mac/scripts/bundle.sh
#
# Signing, in order:
#   DICTATEHUB_ADHOC=1               ad hoc (TCC grants do NOT survive a rebuild)
#   DICTATEHUB_SIGN_IDENTITY=<sha1>  that identity from the default keychains
#                                    (e.g. Apple Development; needs an unlocked
#                                    login keychain, so not over SSH: #3541)
#   default                          the dedicated "DictateHub Local Signing"
#                                    identity, created on first use by
#                                    scripts/signing-setup.sh and unlocked here
#                                    without any prompt
# The default gives a designated requirement that never changes between
# builds (certificate leaf hash), so Microphone and Accessibility grants
# survive rebuilds and reinstalls. If signing with an identity fails the
# bundle is signed ad hoc and SIGNED=adhoc is printed.
set -euo pipefail

cd "$(dirname "$0")/.."
CONFIG=${CONFIG:-release}
APP=build/DictateHub.app
BUNDLE_ID=dev.jakedevar.DictateHub
VERSION=$(tr -d '[:space:]' < VERSION)
BUILD=${DICTATEHUB_BUILD:-$(date -u +%Y%m%d%H%M)}

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

KC="$HOME/Library/Keychains/dictatehub-signing.keychain-db"
PWFILE="$HOME/Library/Keychains/dictatehub-signing.password"
signed=adhoc
if [[ -n ${DICTATEHUB_ADHOC:-} ]]; then
    :
elif [[ -n ${DICTATEHUB_SIGN_IDENTITY:-} ]]; then
    identity=$DICTATEHUB_SIGN_IDENTITY
    echo "==> codesign with identity ${identity:0:8}… (default keychains)"
    if err=$(with_timeout 60 codesign --force --sign "$identity" --identifier "$BUNDLE_ID" \
            --timestamp=none "$APP" 2>&1); then
        signed=identity
    else
        echo "codesign with $identity failed: $err" >&2
        echo "    (over SSH the login keychain is locked; unset DICTATEHUB_SIGN_IDENTITY" >&2
        echo "    to use the dedicated DictateHub signing keychain instead)" >&2
    fi
else
    if identity=$(scripts/signing-setup.sh --print); then
        echo "==> codesign with \"DictateHub Local Signing\" ${identity:0:8}… ($KC)"
        # signing-setup.sh --print left the keychain unlocked; --keychain keeps
        # codesign to this keychain, which is not in the search list.
        if err=$(with_timeout 60 codesign --force --sign "$identity" --keychain "$KC" \
                --identifier "$BUNDLE_ID" --timestamp=none "$APP" 2>&1); then
            signed=identity
        else
            echo "codesign with the DictateHub signing identity failed: $err" >&2
        fi
    else
        echo "signing-setup.sh failed; see above" >&2
    fi
fi
if [[ $signed == adhoc ]]; then
    [[ -n ${DICTATEHUB_ADHOC:-} ]] || echo "    Falling back to ad-hoc signing (TCC grants will NOT survive a rebuild)." >&2
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
