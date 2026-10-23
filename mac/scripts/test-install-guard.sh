#!/bin/bash
# Shell tests for the install signing guard, launch hygiene (#3766), the
# /Applications install + migration and the stale-bundle cleanup (#3807).
# Runs on the Mac (`remote.sh test` runs it); the pure-function and static
# checks also run anywhere. Touches only temp dirs: never /Applications, never
# ~/Applications, never TCC, never the real app (every install.sh run below sets
# DICTATEHUB_DEST_DIR, a stub HOME and recorder commands for lsregister, open and
# defaults). bash 3.2 compatible.
set -uo pipefail
cd "$(dirname "$0")/.."
source scripts/lib-signing.sh
source scripts/lib-launchservices.sh

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

# --- #3807: the destination is /Applications (no sudo), the old path is migrated
expect "default install dir is /Applications" bash -c 'unset DICTATEHUB_DEST_DIR; source scripts/lib-launchservices.sh; [[ $INSTALL_DEST == /Applications/DictateHub.app ]]'
expect "DEST_DIR override only for tests"     grep -q 'DICTATEHUB_DEST_DIR' scripts/lib-launchservices.sh
refute "install.sh does not install to ~/Applications" grep -n 'DEST_DIR="\$HOME/Applications"' scripts/install.sh
refute "install.sh/uninstall.sh never use sudo" grep -nE '(^|[^a-z])sudo ' scripts/install.sh scripts/uninstall.sh scripts/remote.sh scripts/clean-stale.sh
expect "uninstall.sh targets the install dir" grep -q 'DEST=\$INSTALL_DEST' scripts/uninstall.sh
expect "uninstall.sh also removes the old copy" grep -q 'LEGACY_DEST' scripts/uninstall.sh
expect "transcribe-file uses /Applications"   grep -q '^    app=/Applications/DictateHub.app$' scripts/remote.sh
refute "remote.sh has no ~/Applications app path" grep -n 'HOME/Applications' scripts/remote.sh
expect "build dir is a .noindex dir"          grep -q 'REMOTE_DIR="dictate-mac-build/\$ID.noindex"' scripts/remote.sh
expect "sync drops .metadata_never_index"     grep -q 'touch ~/dictate-mac-build/.metadata_never_index' scripts/remote.sh
expect "remote.sh unregisters build bundles"  grep -q 'ls-unregister-build.sh' scripts/remote.sh
expect "install runs clean-stale for this id" grep -q 'clean-stale.sh "\$ID"' scripts/remote.sh
expect "remote.sh has clean-stale"            grep -q '^clean-stale)$' scripts/remote.sh

# --- fixtures shared by the clean-stale and install tests
tmp=$(mktemp -d "${TMPDIR:-/tmp}/dh-guard.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/home"
cat > "$tmp/bin/lsregister" <<'SH'
#!/bin/bash
echo "$@" >> "$FAKE_LS_LOG"
SH
cat > "$tmp/bin/open" <<'SH'
#!/bin/bash
echo "$@" >> "$FAKE_OPEN_LOG"
out=""; prev=""
for a in "$@"; do [[ $prev == --stdout ]] && out=$a; prev=$a; done
case " $* " in *" --unregister-login-item "*) [[ -n $out ]] && echo "login item: ${FAKE_LOGIN_BEFORE:-enabled} -> off" > "$out" ;; esac
exit 0
SH
cat > "$tmp/bin/defaults" <<'SH'
#!/bin/bash
echo "$@" >> "$FAKE_DEFAULTS_LOG"
SH
chmod +x "$tmp/bin/lsregister" "$tmp/bin/open" "$tmp/bin/defaults"
export FAKE_LS_LOG="$tmp/ls.log" FAKE_OPEN_LOG="$tmp/open.log" FAKE_DEFAULTS_LOG="$tmp/defaults.log"

make_bundle() { # make_bundle PATH [BUNDLE_ID]
    mkdir -p "$1/Contents/MacOS"
    cat > "$1/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>${2:-dev.jakedevar.DictateHub}</string>
<key>CFBundleExecutable</key><string>DictateHub</string>
<key>CFBundleShortVersionString</key><string>0</string>
<key>CFBundleVersion</key><string>0</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
PLIST
    printf '#!/bin/sh\nexit 0\n' > "$1/Contents/MacOS/DictateHub"; chmod +x "$1/Contents/MacOS/DictateHub"
}

# --- clean-stale.sh: removes every stale build bundle except the current id's
root="$tmp/dictate-mac-build"
mkdir -p "$tmp/pkg/scripts"
cp scripts/clean-stale.sh scripts/lib-launchservices.sh "$tmp/pkg/scripts/"
make_bundle "$root/cur.noindex/build/DictateHub.app"
make_bundle "$root/old1/build/DictateHub.app"
make_bundle "$root/old2.noindex/build/DictateHub.app"
make_bundle "$root/optionn-src/mac/build/DictateHub.app"
make_bundle "$root/other/build/DictateHub.app" com.example.NotOurs
make_bundle "$tmp/outside/DictateHub.app"
ln -s "$tmp/outside/DictateHub.app" "$root/linked-DictateHub.app" 2>/dev/null || true
mkdir -p "$root/linkdir/build" && ln -s "$tmp/outside/DictateHub.app" "$root/linkdir/build/DictateHub.app"
cs_env() { env DICTATEHUB_BUILD_ROOT="$root" DICTATEHUB_LSREGISTER="$tmp/bin/lsregister" "$@"; }
: > "$tmp/ls.log"
out=$(cs_env bash "$tmp/pkg/scripts/clean-stale.sh" --dry-run cur 2>&1); status=$?
if [[ $status -eq 0 && -d "$root/old1/build/DictateHub.app" && $out == *"dry_run=1"* ]]; then ok; else bad "clean-stale --dry-run must delete nothing ($status: $out)"; fi
refute "clean-stale --dry-run does not unregister" grep -q -- "^-u " "$tmp/ls.log"
out=$(cs_env bash "$tmp/pkg/scripts/clean-stale.sh" cur 2>&1); status=$?
[[ $status -eq 0 ]] && ok || bad "clean-stale failed ($status: $out)"
expect "keeps the current build id's bundle"    test -d "$root/cur.noindex/build/DictateHub.app"
refute "removes a plain-id stale bundle"        test -e "$root/old1/build/DictateHub.app"
refute "removes a .noindex stale bundle"        test -e "$root/old2.noindex/build/DictateHub.app"
refute "removes the optionn-src bundle"         test -e "$root/optionn-src/mac/build/DictateHub.app"
expect "keeps a bundle with another bundle id"  test -d "$root/other/build/DictateHub.app"
expect "never follows a symlink out of the root" test -d "$tmp/outside/DictateHub.app"
expect "unregisters before deleting (old1)"     grep -qF -- "-u $(cd "$root" && pwd -P)/old1/build/DictateHub.app" "$tmp/ls.log"
refute "never unregisters the kept bundle"      grep -qF "cur.noindex" "$tmp/ls.log"
expect "build root is excluded from Spotlight"  test -f "$root/.metadata_never_index"
case $out in *"removed=3 kept=1"*) ok ;; *) bad "clean-stale summary: $out" ;; esac
# the current id may be a plain dir name from an older remote.sh: it is stale now
refute "clean-stale rejects a bad id"           cs_env bash "$tmp/pkg/scripts/clean-stale.sh" '../x'

# --- install.sh: /Applications-style destination and the ~/Applications migration
if [[ $(uname) == Darwin ]]; then
    mkdir -p "$tmp/pkg/scripts" "$tmp/pkg/build"
    cp scripts/install.sh scripts/lib-signing.sh scripts/lib-launchservices.sh "$tmp/pkg/scripts/"
    app="$tmp/pkg/build/DictateHub.app"
    make_bundle "$app"
    cp /usr/bin/true "$app/Contents/MacOS/DictateHub"
    install_env() { # install_env [FAKE_LOGIN_BEFORE=..] -- args to install.sh
        env HOME="$tmp/home" DICTATEHUB_DEST_DIR="$tmp/apps" DICTATEHUB_LEGACY_DEST="$tmp/home/Applications/DictateHub.app" \
            DICTATEHUB_LSREGISTER="$tmp/bin/lsregister" DICTATEHUB_OPEN="$tmp/bin/open" DICTATEHUB_DEFAULTS="$tmp/bin/defaults" \
            "$@"
    }
    if codesign --force --sign - --identifier dev.jakedevar.DictateHub "$app" 2>/dev/null; then
        mkdir -p "$tmp/apps"
        out=$(install_env bash "$tmp/pkg/scripts/install.sh" --no-launch 2>&1); status=$?
        if [[ $status -ne 0 && $out == *"ad-hoc signed"* ]]; then ok; else bad "install.sh must refuse an ad-hoc bundle (status $status: $out)"; fi
        [[ ! -e "$tmp/apps/DictateHub.app" ]] && ok || bad "refused install must not write the app"
        : > "$tmp/ls.log"; : > "$tmp/open.log"; : > "$tmp/defaults.log"
        out=$(install_env bash "$tmp/pkg/scripts/install.sh" --no-launch --allow-adhoc 2>&1); status=$?
        if [[ $status -eq 0 && -d "$tmp/apps/DictateHub.app" ]]; then ok; else bad "--allow-adhoc must install (status $status: $out)"; fi
        expect "installed copy is owned by the user"    test -O "$tmp/apps/DictateHub.app"
        expect "install registers the new bundle"       grep -qF -- "-f $tmp/apps/DictateHub.app" "$tmp/ls.log"
        refute "no legacy copy: no login-item migration" grep -q unregister-login-item "$tmp/open.log"
        refute "install left no staging dir"            test -e "$tmp/apps/.DictateHub.app.installing"

        # migration: an old ~/Applications copy with the login item on
        make_bundle "$tmp/home/Applications/DictateHub.app"
        : > "$tmp/ls.log"; : > "$tmp/open.log"; : > "$tmp/defaults.log"
        out=$(FAKE_LOGIN_BEFORE=enabled install_env bash "$tmp/pkg/scripts/install.sh" --no-launch --allow-adhoc 2>&1); status=$?
        [[ $status -eq 0 ]] && ok || bad "migration install failed ($status: $out)"
        refute "migration removes ~/Applications copy"  test -e "$tmp/home/Applications/DictateHub.app"
        expect "migration unregisters the old path"     grep -qF -- "-u $tmp/home/Applications/DictateHub.app" "$tmp/ls.log"
        expect "migration unregisters the old login item (via open)" grep -q -- "--unregister-login-item" "$tmp/open.log"
        expect "old open targeted the old bundle"       grep -qF "$tmp/home/Applications/DictateHub.app" "$tmp/open.log"
        expect "login item was on: first-install flag reset" grep -q 'delete dev.jakedevar.DictateHub loginItemConfigured' "$tmp/defaults.log"
        expect "new bundle is in place after migration" test -d "$tmp/apps/DictateHub.app"

        # migration with the login item off by the user: stays off
        make_bundle "$tmp/home/Applications/DictateHub.app"
        : > "$tmp/ls.log"; : > "$tmp/open.log"; : > "$tmp/defaults.log"
        out=$(FAKE_LOGIN_BEFORE=off install_env bash "$tmp/pkg/scripts/install.sh" --no-launch --allow-adhoc 2>&1); status=$?
        [[ $status -eq 0 && ! -e "$tmp/home/Applications/DictateHub.app" ]] && ok || bad "migration (login item off) failed ($status: $out)"
        refute "login item off by the user is not re-armed" grep -q loginItemConfigured "$tmp/defaults.log"

        # an unwritable destination is an error, not a sudo prompt
        mkdir -p "$tmp/ro" && chmod a-w "$tmp/ro"
        if [[ ! -w "$tmp/ro" ]]; then
            out=$(env HOME="$tmp/home" DICTATEHUB_DEST_DIR="$tmp/ro" DICTATEHUB_LEGACY_DEST="$tmp/none/DictateHub.app" \
                DICTATEHUB_LSREGISTER="$tmp/bin/lsregister" DICTATEHUB_OPEN="$tmp/bin/open" DICTATEHUB_DEFAULTS="$tmp/bin/defaults" \
                bash "$tmp/pkg/scripts/install.sh" --no-launch --allow-adhoc 2>&1); status=$?
            if [[ $status -ne 0 && $out == *"not writable"* ]]; then ok; else bad "unwritable destination must fail clearly ($status: $out)"; fi
        fi
        chmod u+w "$tmp/ro"
    else
        echo "note: codesign not usable here; skipped the install.sh checks" >&2
    fi
else
    echo "note: not macOS; skipped the install.sh checks" >&2
fi

echo "install-guard tests: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
