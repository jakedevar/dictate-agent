#!/bin/bash
# Build, test, bundle and launch DictateHub on the Mac from the hub.
#
#   mac/scripts/remote.sh build            # swift build
#   mac/scripts/remote.sh test             # swift test
#   mac/scripts/remote.sh bundle           # mac/scripts/bundle.sh (signed .app)
#   mac/scripts/remote.sh run [SECONDS]    # bundle, then `open` it; with SECONDS
#                                          # it runs in smoke mode and quits itself
#   mac/scripts/remote.sh probe            # bundle, then check the bundled app's
#                                          # Test connection against an nc stub
#   mac/scripts/remote.sh stop             # quit a DictateHub launched from this dir
#   mac/scripts/remote.sh signing-setup    # create/verify the dedicated signing keychain
#   mac/scripts/remote.sh dr-check         # bundle twice; the designated requirement
#                                          # must match while the cdhash differs
#   mac/scripts/remote.sh install [--no-launch] [--allow-adhoc]
#                                          # bundle + /Applications/DictateHub.app
#                                          # (no sudo; migrates ~/Applications away),
#                                          # launched once (turns on Open at login),
#                                          # then clean-stale. Refuses an ad-hoc
#                                          # bundle unless --allow-adhoc
#   mac/scripts/remote.sh uninstall [--purge] [--remove-signing-keychain]
#   mac/scripts/remote.sh file-probe       # bundle, file mode against a stub hub
#   mac/scripts/remote.sh transcribe-file WAV [--insert] [--app NAME]
#                                          # send a local WAV through the installed
#                                          # app's client to the REAL hub (e2e)
#   mac/scripts/remote.sh shell CMD...     # run CMD in the remote package dir
#   mac/scripts/remote.sh clean            # delete this build dir on the Mac
#   mac/scripts/remote.sh clean-stale       # unregister + delete every DictateHub.app build
#                                          # bundle under ~/dictate-mac-build/ except this
#                                          # build id's (never the installed app)
#
# Every command first rsyncs mac/ to the Mac. The package lives in
# ~/dictate-mac-build/<id>.noindex/ (id = $DICTATE_MAC_BUILD_ID, else
# $RSI_SESSION_ID, else "manual"), and the .app in its build/. The .noindex suffix
# and a .metadata_never_index file keep Spotlight from indexing build bundles,
# and bundles a run launched are removed from LaunchServices afterwards, so a
# stale ad-hoc build never shows up in Spotlight or Launchpad (#3807). Only
# `install` (and clean-stale, inside ~/dictate-mac-build) writes outside that dir.
# Host: $DICTATE_MAC_HOST (default mac-mini).
set -euo pipefail

source "$(dirname "$0")/lib-signing.sh"
HOST=${DICTATE_MAC_HOST:-mac-mini}
ID=${DICTATE_MAC_BUILD_ID:-${RSI_SESSION_ID:-manual}}
[[ $ID =~ ^[A-Za-z0-9._-]+$ ]] || { echo "bad build id: $ID" >&2; exit 2; }
REMOTE_DIR="dictate-mac-build/$ID.noindex"  # relative to the remote $HOME
HERE=$(cd "$(dirname "$0")/.." && pwd)

ssh_mac() { ssh -o BatchMode=yes -o ConnectTimeout=15 "$HOST" "$@"; }

sync() {
    ssh_mac "mkdir -p ~/$REMOTE_DIR && touch ~/dictate-mac-build/.metadata_never_index ~/$REMOTE_DIR/.metadata_never_index"
    rsync -a --delete --exclude .build/ --exclude build/ --exclude .swiftpm/ \
        "$HERE/" "$HOST:$REMOTE_DIR/"
}

# Run a command line in the remote package dir with a login-ish PATH.
remote() { ssh_mac "cd ~/$REMOTE_DIR && export PATH=/usr/bin:/bin:/usr/sbin:/sbin:\$PATH && $*"; }

app_pids() {
    # Only processes running this build dir's bundle executable.
    remote "pgrep -f \"\$HOME/$REMOTE_DIR/build/DictateHub.app/Contents/MacOS/DictateHub\" || true"
}

# Remove this build dir's bundle from LaunchServices (best effort), so a bundle
# a run launched is never offered by Spotlight or Launchpad (#3807).
unregister_build() { remote scripts/ls-unregister-build.sh >/dev/null 2>&1 || true; }
UNREG=0
trap 'if [[ $UNREG == 1 ]]; then unregister_build; fi' EXIT

cmd=${1:-}
shift || true
case $cmd in
build)
    sync
    remote swift build "$@"
    ;;
test)
    sync
    UNREG=1
    remote swift test "$@"
    # Shell tests: install ad-hoc guard, launch hygiene (run on the Mac, which
    # has codesign; they touch only temp dirs).
    remote scripts/test-install-guard.sh
    remote scripts/test-local-transcribe.sh
    ;;
bundle)
    sync
    remote scripts/bundle.sh
    ;;
run)
    secs=${1:-}
    sync
    [[ -n $secs ]] && UNREG=1  # a smoke run quits; a normal run stays registered until stop
    remote scripts/bundle.sh
    if [[ -n $(app_pids) ]]; then echo "already running: $(app_pids | tr '\n' ' ')" >&2; exit 1; fi
    log="build/launch.log"
    if [[ -n $secs ]]; then
        [[ $secs =~ ^[0-9]+$ ]] || { echo "SECONDS must be an integer" >&2; exit 2; }
        remote "rm -f $log && open -n --stdout $log --stderr $log build/DictateHub.app --args --smoke $secs"
        sleep $((secs + 3))
        remote "cat $log"
        left=$(app_pids)
        if [[ -n $left ]]; then echo "still running after smoke window: $left" >&2; exit 1; fi
        remote "grep -q 'started: status item installed=true' $log" \
            && echo "SMOKE OK: launched, status item installed, quit by itself"
    else
        remote "rm -f $log && open -n --stdout $log --stderr $log build/DictateHub.app"
        sleep 3
        remote "cat $log"
        echo "pid(s): $(app_pids | tr '\n' ' ')   stop with: $0 stop"
    fi
    ;;
probe)
    sync
    UNREG=1
    remote scripts/bundle.sh
    remote scripts/probe.sh
    ;;
stop)
    UNREG=1
    pids=$(app_pids)
    if [[ -z $pids ]]; then echo "not running"; exit 0; fi
    # SIGTERM to exactly the PIDs of this build dir's executable.
    remote "kill $(echo "$pids" | tr '\n' ' ')"
    echo "stopped: $pids"
    ;;
signing-setup)
    sync
    remote scripts/signing-setup.sh "$@"
    ;;
dr-check)
    UNREG=1
    # Two bundles with different build numbers (so different bytes and
    # cdhash) must report the same designated requirement.
    sync
    remote "mkdir -p build"
    for n in 1 2; do
        remote "DICTATEHUB_BUILD=drcheck$n$(date +%s) scripts/bundle.sh >/dev/null 2>build/dr$n.err || { cat build/dr$n.err; exit 1; };"\
" codesign -d -r- build/DictateHub.app 2>&1 | grep designated > build/dr$n.txt;"\
" codesign -dvvv build/DictateHub.app 2>&1 | grep -E '^CDHash=' > build/cd$n.txt"
    done
    remote "for n in 1 2; do echo \"build \$n: \$(cat build/cd\$n.txt)\"; echo \"         \$(cat build/dr\$n.txt)\"; done"
    remote "cmp -s build/dr1.txt build/dr2.txt && ! cmp -s build/cd1.txt build/cd2.txt && grep -q 'certificate leaf' build/dr1.txt" \
        && echo "DR STABLE: same certificate-based designated requirement across two different builds" \
        || { echo "DR CHECK FAILED" >&2; exit 1; }
    ;;
install)
    sync
    # An install never falls back to an ad-hoc bundle silently: bundle.sh
    # fails unless --allow-adhoc is passed (install.sh refuses it too).
    if args_allow_adhoc "$@"; then
        remote scripts/bundle.sh
    else
        remote DICTATEHUB_REQUIRE_SIGNED=1 scripts/bundle.sh
    fi
    remote scripts/install.sh "$@"
    # The build bundles are done with: drop every stale one but this build id's.
    remote scripts/clean-stale.sh "$ID"
    ;;
clean-stale)
    sync
    remote scripts/clean-stale.sh "$@" "$ID"
    ;;
uninstall)
    sync
    remote scripts/uninstall.sh "$@"
    ;;
file-probe)
    sync
    UNREG=1
    remote scripts/bundle.sh
    remote scripts/file-probe.sh
    ;;
transcribe-file)
    # The PM's end-to-end: a local WAV → the installed app's file mode → the
    # hub API through the tunnel, with the app's saved endpoint and token.
    wav=${1:-}
    [[ -f $wav ]] || { echo "usage: $0 transcribe-file WAV [--insert] [--app NAME]" >&2; exit 2; }
    shift
    sync
    remote "mkdir -p build/e2e && rm -f build/e2e/out.log"
    scp -q "$wav" "$HOST:$REMOTE_DIR/build/e2e/clip.wav"
    app=/Applications/DictateHub.app
    remote "[ -d $app ] || { echo 'not installed; run: $0 install' >&2; exit 1; }"
    extra=$(printf ' %q' "$@")
    # -g: DictateHub must not become frontmost (with --insert, Cmd+V goes to
    # the app that is). -W: wait for it to exit.
    remote "open -W -n -g --stdout build/e2e/out.log --stderr build/e2e/out.log $app --args"\
" --transcribe-file \$PWD/build/e2e/clip.wav$extra; cat build/e2e/out.log"
    remote "grep -q '^DICTATEHUB_RESULT .*\"ok\":true' build/e2e/out.log"
    ;;
shell)
    sync
    remote "$@"
    ;;
clean)
    remote scripts/ls-unregister-build.sh >/dev/null 2>&1 || true
    ssh_mac "rm -rf ~/$REMOTE_DIR"
    ;;
*)
    sed -n '2,34p' "$0" >&2
    exit 2
    ;;
esac
