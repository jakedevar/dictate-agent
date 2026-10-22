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
#   mac/scripts/remote.sh shell CMD...     # run CMD in the remote package dir
#   mac/scripts/remote.sh clean            # delete this build dir on the Mac
#
# Every command first rsyncs mac/ to the Mac. Nothing is installed: the
# package lives in ~/dictate-mac-build/<id>/ (id = $DICTATE_MAC_BUILD_ID,
# else $RSI_SESSION_ID, else "manual"), and the .app stays in its build/.
# Host: $DICTATE_MAC_HOST (default mac-mini).
set -euo pipefail

HOST=${DICTATE_MAC_HOST:-mac-mini}
ID=${DICTATE_MAC_BUILD_ID:-${RSI_SESSION_ID:-manual}}
[[ $ID =~ ^[A-Za-z0-9._-]+$ ]] || { echo "bad build id: $ID" >&2; exit 2; }
REMOTE_DIR="dictate-mac-build/$ID"  # relative to the remote $HOME
HERE=$(cd "$(dirname "$0")/.." && pwd)

ssh_mac() { ssh -o BatchMode=yes -o ConnectTimeout=15 "$HOST" "$@"; }

sync() {
    ssh_mac "mkdir -p ~/$REMOTE_DIR"
    rsync -a --delete --exclude .build/ --exclude build/ --exclude .swiftpm/ \
        "$HERE/" "$HOST:$REMOTE_DIR/"
}

# Run a command line in the remote package dir with a login-ish PATH.
remote() { ssh_mac "cd ~/$REMOTE_DIR && export PATH=/usr/bin:/bin:/usr/sbin:/sbin:\$PATH && $*"; }

app_pids() {
    # Only processes running this build dir's bundle executable.
    remote "pgrep -f \"\$HOME/$REMOTE_DIR/build/DictateHub.app/Contents/MacOS/DictateHub\" || true"
}

cmd=${1:-}
shift || true
case $cmd in
build)
    sync
    remote swift build "$@"
    ;;
test)
    sync
    remote swift test "$@"
    ;;
bundle)
    sync
    remote scripts/bundle.sh
    ;;
run)
    secs=${1:-}
    sync
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
    remote scripts/bundle.sh
    remote scripts/probe.sh
    ;;
stop)
    pids=$(app_pids)
    if [[ -z $pids ]]; then echo "not running"; exit 0; fi
    # SIGTERM to exactly the PIDs of this build dir's executable.
    remote "kill $(echo "$pids" | tr '\n' ' ')"
    echo "stopped: $pids"
    ;;
shell)
    sync
    remote "$@"
    ;;
clean)
    ssh_mac "rm -rf ~/$REMOTE_DIR"
    ;;
*)
    sed -n '2,17p' "$0" >&2
    exit 2
    ;;
esac
