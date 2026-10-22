#!/usr/bin/env bash
# Isolated hub + Mac UDS end-to-end check. Private XDG paths, capture disabled,
# alternate hub port, throwaway Mac socket/token/clip; cleanup on both hosts.
# Never installs the real tunnel or touches the live daemon/config/token.
# Env: DICTATED, DICTATE (CPU binaries), E2E_MODEL (read-only model path).
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
host="${DICTATE_MAC_HOST:-mac-mini}"
port=17313
while [ $# -gt 0 ]; do
    case "$1" in
        --host) host="$2"; shift 2 ;;
        --port) port="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
dictated="${DICTATED:-$repo/target/release/dictated}"
dictate="${DICTATE:-$repo/target/release/dictate}"
model="${E2E_MODEL:-$HOME/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin}"
wav="$repo/crates/dictated/tests/fixtures/e2e/short.wav"
unit_file="$repo/systemd/dictate-mac-tunnel.service"

# The hub may have no nc; bash's /dev/tcp is enough for "is it listening".
local_open() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
say()  { printf 'mac-smoke| %s\n' "$*"; }
fail() { printf 'mac-smoke| FAIL: %s\n' "$*" >&2; exit 1; }

[ "$port" != 7313 ] || fail "port 7313 belongs to the live API; pick another"
[ -x "$dictated" ] && [ -x "$dictate" ] || fail "build first: $dictated / $dictate"
[ -f "$model" ] || fail "no speech model at $model (this script never downloads one)"
ssh -o BatchMode=yes -o ConnectTimeout=10 "$host" true || fail "ssh $host does not work non-interactively"
if ssh -o BatchMode=yes "$host" "nc -z 127.0.0.1 $port" 2>/dev/null; then
    fail "something already listens on $host:127.0.0.1:$port"
fi

root="$(mktemp -d /tmp/dmt-smoke.XXXXXX)"
t0="$(date +%s)"
unit="dmt-smoke-$$"
remote=""
pids=()
cleanup() {
    systemctl --user stop "$unit" 2>/dev/null || true
    systemctl --user reset-failed "$unit" 2>/dev/null || true
    for p in "${pids[@]}"; do
        if kill -0 "$p" 2>/dev/null; then kill -TERM "$p" 2>/dev/null || true; wait "$p" 2>/dev/null || true; fi
    done
    if [ -n "$remote" ]; then ssh -o BatchMode=yes "$host" "rm -rf '$remote'" || true; fi
    rm -rf "$root"
}
trap cleanup EXIT

# --- isolated daemon(s) -----------------------------------------------------
# $1 = name, $2 = API port. Each gets its own runtime/config/data.
start_daemon() {
    local name="$1" api_port="$2" d="$root/$1"
    mkdir -p "$d/run" "$d/config/dictate-agent" "$d/data"
    chmod 700 "$d/run"
    cat > "$d/config/dictate-agent/config.toml" <<CFG
[whisper]
model = "large-v3-turbo"
model_path = "$model"
device = "cpu"
language = "en"
[audio]
capture = false
pre_roll_ms = 0
[grammar]
enabled = false
[notifications]
enabled = false
[hotkey]
enabled = false
[api]
enabled = true
bind = "127.0.0.1:$api_port"
allowed_hosts = ["127.0.0.1:7313"]
token_file = "$d/data/api-token"
CFG
    (
        export XDG_RUNTIME_DIR="$d/run" XDG_CONFIG_HOME="$d/config" XDG_DATA_HOME="$d/data"
        export DICTATE_SOCKET="$d/run/dictate-agent/dictated.sock"
        unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
        "$dictated" --api-token >/dev/null 2>&1
        exec env RUST_LOG=info "$dictated" > "$d/dictated.log" 2>&1
    ) &
    pids+=("$!")
    for _ in $(seq 1 300); do
        [ -S "$d/run/dictate-agent/dictated.sock" ] && return 0
        kill -0 "${pids[-1]}" 2>/dev/null || { tail -20 "$d/dictated.log" >&2; fail "$name exited during startup"; }
        sleep 0.1
    done
    fail "$name did not bind its socket"
}
cli() { # $1 = daemon name, rest = dictate args
    local d="$root/$1"; shift
    XDG_RUNTIME_DIR="$d/run" DICTATE_SOCKET="$d/run/dictate-agent/dictated.sock" "$dictate" "$@"
}

say "starting isolated dictated A (API 127.0.0.1:$port)"
start_daemon a "$port"
for _ in $(seq 1 50); do local_open "$port" && break; sleep 0.1; done
local_open "$port" || fail "A's API is not listening"
ss -Hltn "sport = :$port" | awk '{print $4}' | grep -qvx "127.0.0.1:$port" \
    && fail "A listens beyond loopback: $(ss -Hltn "sport = :$port")"

# --- robustness: B's API port is taken by A ----------------------------------
say "starting isolated dictated B on the SAME API port (must keep serving)"
start_daemon b "$port"
cli b status >/dev/null || fail "B's socket does not answer with its API port taken"
grep -q "network API is DOWN" "$root/b/dictated.log" || fail "B did not log the API as down"
doctor_b="$(cli b doctor --quick 2>&1 || true)"
printf '%s\n' "$doctor_b" | grep -q "Network API" || fail "doctor has no Network API line"
printf '%s\n' "$doctor_b" | grep -i "Network API" | grep -q "DOWN" || fail "doctor does not show B's API as down"
b_text="$(cli b transcribe "$wav" 2>/dev/null)" || fail "B cannot transcribe with its API down"
[ -n "$b_text" ] || fail "B transcribed nothing"
say "  B: socket ok, doctor 'Network API ... DOWN', local transcribe ok"

# --- tunnel: same wrapper as the unit, throwaway Mac socket -----------------
grep -qx 'ExecStart=%h/.local/bin/dictate-mac-tunnel' "$unit_file" || fail "unit wrapper missing"
grep -qx 'RestartPreventExitStatus=78' "$unit_file" || fail "unit retries safety refusal"
remote="$(ssh -o BatchMode=yes "$host" 'mktemp -d /tmp/dictatehub-smoke.XXXXXX')"
wrapper="$repo/scripts/dictate-mac-tunnel"
DICTATE_MAC_HOST="$host" DICTATE_MAC_SOCKET="$remote/hub.sock" \
    DICTATE_HUB_API_PORT="$port" "$wrapper" --check
systemd-run --user --quiet --collect --unit="$unit" \
    -p Restart=always -p RestartSec=2 -p RestartPreventExitStatus=78 \
    --setenv=DICTATE_MAC_HOST="$host" \
    --setenv=DICTATE_HUB_API_PORT="$port" \
    --setenv=DICTATE_MAC_SOCKET="$remote/hub.sock" "$wrapper"
for _ in {1..100}; do
    timeout 5 ssh -o BatchMode=yes "$host" "test -S '$remote/hub.sock'" && break
    sleep 0.2
done
ssh -o BatchMode=yes "$host" "test -S '$remote/hub.sock' && test \"\$(stat -f %Lp '$remote')\" = 700" || fail 'private socket missing'
if ssh -o BatchMode=yes "$host" "lsof -nP -iTCP:$port -sTCP:LISTEN"; then fail 'unexpected Mac TCP listener'; fi
say 'Mac private UDS ready; no TCP listener on the test port'

# --- token provisioning, then curl on the Mac --------------------------------
XDG_DATA_HOME="$root/a/data" XDG_CONFIG_HOME="$root/a/config" \
    DICTATED="$dictated" DICTATED_CONFIG="$root/a/config/dictate-agent/config.toml" \
    "$repo/scripts/mac-provision-token.sh" --host "$host" --remote-dir "$remote" \
    | sed 's/^/mac-smoke|   /'
ssh -o BatchMode=yes "$host" "cat > '$remote/short.wav'" < "$wav"

# The header is built in a 0600 file on the Mac so the token is on no argv.
remote_curl() { # $1 = token mode (good|none|wrong), rest = curl args
    local mode="$1" args; shift
    args="$(printf '%q ' "$@")"
    ssh -o BatchMode=yes "$host" "cd '$remote' && umask 077 &&
        case $mode in
          good)  printf 'Authorization: Bearer %s\n' \"\$(cat api-token)\" > hdr ;;
          wrong) printf 'Authorization: Bearer dct1_%s\n' AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA > hdr ;;
          none)  : > hdr ;;
        esac &&
        curl -sS --max-time 30 --unix-socket '$remote/hub.sock' -H @hdr $args; rc=\$?; rm -f hdr; exit \$rc"
}

status_json="$(remote_curl good "http://127.0.0.1:$port/v1/status")"
printf '%s' "$status_json" | python3 -I -c 'import json,sys; d=json.load(sys.stdin); assert d["type"]=="status", d' \
    || fail "status through the tunnel: $status_json"
say "  GET /v1/status with the token: type=status"
for mode in none wrong; do
    code="$(remote_curl "$mode" -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/v1/status")"
    [ "$code" = 401 ] || fail "$mode token got HTTP $code, expected 401"
    say "  GET /v1/status with $mode token: $code"
done
reply="$(remote_curl good -H 'Content-Type: audio/wav' --data-binary @short.wav "http://127.0.0.1:$port/v1/transcribe?app=smoke")"
printf '%s' "$reply" | python3 -I -c '
import json, sys
d = json.load(sys.stdin)
assert d["type"] == "transcript", d
assert d["route"] == "type", d
assert d["text"].strip(), d
print("mac-smoke|   POST /v1/transcribe: type=transcript, route=type, %d chars" % len(d["text"]))
' || fail "transcribe through the tunnel: $reply"
code="$(remote_curl none -o /dev/null -w '%{http_code}' -H 'Content-Type: audio/wav' --data-binary @short.wav "http://127.0.0.1:$port/v1/transcribe")"
[ "$code" = 401 ] || fail "an unauthenticated upload got HTTP $code"
say "  POST /v1/transcribe without a token: $code"

# --- the installed app's file mode uses this same UDS client ----------------
# Explicit test endpoint/token; never modifies saved settings or the real token.
ssh -o BatchMode=yes "$host" "open -W -n -g --stdout '$remote/app.log' --stderr '$remote/app.log' \
    \"\$HOME/Applications/DictateHub.app\" --args --transcribe-file '$remote/short.wav' \
    --endpoint 'unix:$remote/hub.sock' --token-dir '$remote'"
ssh -o BatchMode=yes "$host" "grep -q '^DICTATEHUB_RESULT .*\"ok\":true' '$remote/app.log'" || fail 'app UDS file mode failed'
say 'installed app file mode over UDS: ok=true'

# --- killed tunnel restarts and stale socket is removed --------------------
systemctl --user kill --signal=KILL "$unit"
sleep 1
code=0
for _ in {1..60}; do
    code=$(remote_curl good -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/v1/status" 2>/dev/null || true)
    [[ $code = 200 ]] && break
    sleep 0.5
done
[[ $code = 200 ]] || fail 'tunnel did not recover after SIGKILL'
say 'killed test tunnel: restarted, stale socket replaced, status 200 again'

# --- stopping leaves no usable endpoint ------------------------------------
systemctl --user stop "$unit"
if remote_curl good -o /dev/null "http://127.0.0.1:$port/v1/status" 2>/dev/null; then fail 'socket still serves after stop'; fi
say 'tunnel stopped; socket cannot carry traffic'
say 'PASS (cleanup removes both isolated daemons and all throwaway Mac files)'
