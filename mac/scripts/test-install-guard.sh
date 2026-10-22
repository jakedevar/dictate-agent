#!/bin/bash
# Shell tests for the install signing guard and launch hygiene (#3766).
# Runs on the Mac (`remote.sh test` runs it); the pure-function and static
# checks also run anywhere. Touches only temp dirs: never ~/Applications, never
# TCC, never the real app. bash 3.2 compatible.
set -uo pipefail
cd "$(dirname "$0")/.."
source scripts/lib-signing.sh

pass=0
fail=0
ok()  { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); echo "FAIL: $1" >&2; }
expect() { # expect "name" cmd...   (cmd must succeed)
    local name=$1; shift
    if "$@"; then ok; else bad "$name"; fi
}
refute() { # refute "name" cmd...   (cmd must fail)
    local name=$1; shift
    if "$@"; then bad "$name"; else ok; fi
}

# --- designated_is_stable
expect "certificate leaf is stable" designated_is_stable 'designated => identifier "dev.jakedevar.DictateHub" and certificate leaf = H"27a5d983"'
expect "apple anchor is stable"     designated_is_stable 'designated => anchor apple generic and identifier "x"'
refute "cdhash is not stable"       designated_is_stable 'designated => cdhash H"8e2ca256"'
refute "empty is not stable"        designated_is_stable ''

# --- args_allow_adhoc
expect "--allow-adhoc found"        args_allow_adhoc --no-launch --allow-adhoc
refute "no flag"                    args_allow_adhoc --no-launch
refute "no args"                    args_allow_adhoc
refute "similar flag"               args_allow_adhoc --allow-adhocish

# --- bundle_signing_guard
refute "adhoc + require-signed fails" env DICTATEHUB_REQUIRE_SIGNED=1 bash -c 'source scripts/lib-signing.sh; bundle_signing_guard adhoc 2>/dev/null'
expect "identity + require-signed ok" env DICTATEHUB_REQUIRE_SIGNED=1 bash -c 'source scripts/lib-signing.sh; bundle_signing_guard identity'
expect "adhoc without the guard ok"   env -u DICTATEHUB_REQUIRE_SIGNED bash -c 'source scripts/lib-signing.sh; bundle_signing_guard adhoc'

# --- bundle.sh wires the guard before any ad-hoc codesign
guard_line=$(grep -n 'bundle_signing_guard adhoc' scripts/bundle.sh | head -1 | cut -d: -f1)
adhoc_line=$(grep -n 'codesign --force --sign - ' scripts/bundle.sh | head -1 | cut -d: -f1)
if [[ -n $guard_line && -n $adhoc_line && $guard_line -lt $adhoc_line ]]; then ok; else bad "bundle.sh guard precedes the ad-hoc codesign"; fi

# --- remote.sh install sets the guard unless --allow-adhoc
if grep -q 'DICTATEHUB_REQUIRE_SIGNED=1 scripts/bundle.sh' scripts/remote.sh \
    && grep -q 'args_allow_adhoc' scripts/remote.sh; then ok; else bad "remote.sh install requires a signed bundle"; fi

# --- launch hygiene: the app is only ever started through LaunchServices
# (`open`), so macOS attributes it to DictateHub, never to sshd or a shell.
# Allowed mentions of the executable path: cp into the bundle, pgrep/EXE for
# finding a running copy, and this test.
offenders=$(grep -n 'Contents/MacOS' scripts/*.sh \
    | grep -v '^scripts/test-install-guard.sh' \
    | grep -v 'pgrep -f' | grep -v 'EXE=' | grep -v 'mkdir -p' | grep -v 'cp "\$BIN"' || true)
if [[ -z $offenders ]]; then ok; else bad "a script runs the bundle executable directly: $offenders"; fi
launchers=$(grep -c 'open ' scripts/install.sh scripts/remote.sh scripts/probe.sh scripts/file-probe.sh scripts/uninstall.sh | awk -F: '{s += $2} END {print s}')
if [[ $launchers -ge 5 ]]; then ok; else bad "expected the scripts to launch through open (found $launchers)"; fi

# --- install.sh on the Mac: ad-hoc refused, ad-hoc allowed only with the flag
if [[ $(uname) == Darwin ]]; then
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/dh-guard.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    mkdir -p "$tmp/pkg/scripts" "$tmp/pkg/build/DictateHub.app/Contents/MacOS" "$tmp/home"
    cp scripts/install.sh scripts/lib-signing.sh "$tmp/pkg/scripts/"
    app="$tmp/pkg/build/DictateHub.app"
    cp /usr/bin/true "$app/Contents/MacOS/DictateHub"
    cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.jakedevar.DictateHub</string>
<key>CFBundleExecutable</key><string>DictateHub</string>
<key>CFBundleShortVersionString</key><string>0</string>
<key>CFBundleVersion</key><string>0</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
PLIST
    if codesign --force --sign - --identifier dev.jakedevar.DictateHub "$app" 2>/dev/null; then
        out=$(HOME="$tmp/home" bash "$tmp/pkg/scripts/install.sh" --no-launch 2>&1); status=$?
        if [[ $status -ne 0 && $out == *"ad-hoc signed"* ]]; then ok; else bad "install.sh must refuse an ad-hoc bundle (status $status: $out)"; fi
        [[ ! -e "$tmp/home/Applications/DictateHub.app" ]] && ok || bad "refused install must not write the app"
        out=$(HOME="$tmp/home" bash "$tmp/pkg/scripts/install.sh" --no-launch --allow-adhoc 2>&1); status=$?
        if [[ $status -eq 0 && -d "$tmp/home/Applications/DictateHub.app" ]]; then ok; else bad "--allow-adhoc must install (status $status: $out)"; fi
    else
        echo "note: codesign not usable here; skipped the install.sh ad-hoc checks" >&2
    fi
else
    echo "note: not macOS; skipped the install.sh ad-hoc checks" >&2
fi

echo "install-guard tests: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
