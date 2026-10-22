#!/usr/bin/env bash
# Throwaway end-to-end check of the Mac app's hub side (`just mac-tunnel-smoke`):
#
#   isolated dictated, API on 127.0.0.1:$PORT
#     -> the REAL ExecStart of systemd/dictate-mac-tunnel.service, run as a
#        transient systemd --user unit with the ports overridden to $PORT
#     -> scripts/mac-provision-token.sh into a temp dir on the Mac
#     -> curl on the Mac: status 200, no/wrong token 401, a WAV -> transcript
#     -> the Mac listens on loopback only
#   plus: a second isolated dictated whose API port is taken keeps serving.
#
# ISOLATION: private XDG_RUNTIME_DIR/XDG_CONFIG_HOME/XDG_DATA_HOME/
# DICTATE_SOCKET; [audio] capture = false; no display, no notifications; CPU
# Whisper; port 7313 (the live API's) is refused. The real tunnel unit is never
# installed or enabled: the transient unit has its own name and is stopped on
# exit, as are both daemons. On the Mac only a mktemp directory under /tmp is
# written, and it is removed on exit. Never prints a token.
#
# Usage: scripts/mac-tunnel-smoke.sh [--host mac-mini] [--port 17313]
# Env:   DICTATED / DICTATE (default target/release/{dictated,dictate}),
#        E2E_MODEL (default the installed large-v3-turbo; only read).
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

# --- the tunnel: the unit file's own ExecStart, ports overridden ------------
exec_start="$(python3 -I - "$unit_file" <<'PY'
import sys
lines, buf = [], ""
for raw in open(sys.argv[1]):
    line = raw.rstrip("\n")
    if line.endswith("\\"):
        buf += line[:-1] + " "
        continue
    lines.append(buf + line); buf = ""
cmd = [l.split("=", 1)[1] for l in lines if l.startswith("ExecStart=")]
assert len(cmd) == 1, cmd
print(cmd[0])
PY
)"
say "transient unit $unit: ${exec_start//\$\{DICTATE_MAC_HOST\}/$host}"
# shellcheck disable=SC2086  # word-split the unit's command line, as systemd does
systemd-run --user --quiet --collect --unit="$unit" \
    -p Restart=always -p RestartSec=10 \
    --setenv=DICTATE_MAC_HOST="$host" \
    --setenv=DICTATE_HUB_API_PORT="$port" \
    --setenv=DICTATE_MAC_PORT="$port" \
    $exec_start
for _ in $(seq 1 100); do
    ssh -o BatchMode=yes "$host" "nc -z 127.0.0.1 $port" 2>/dev/null && break
    sleep 0.2
done
ssh -o BatchMode=yes "$host" "nc -z 127.0.0.1 $port" 2>/dev/null || fail "the tunnel never came up ($(systemctl --user is-active "$unit"))"
listen="$(ssh -o BatchMode=yes "$host" "lsof -nP -iTCP:$port -sTCP:LISTEN" | awk 'NR>1 {print $9}' | sort -u)"
say "  Mac listens on: $(echo $listen)"
[ -n "$listen" ] || fail "no listener seen on the Mac"
printf '%s\n' "$listen" | grep -qvx "127.0.0.1:$port" && fail "the Mac listens beyond loopback: $listen"

# --- token provisioning, then curl on the Mac --------------------------------
remote="$(ssh -o BatchMode=yes "$host" 'mktemp -d /tmp/dictatehub-smoke.XXXXXX')"
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
        curl -sS -H @hdr $args; rc=\$?; rm -f hdr; exit \$rc"
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

# --- Restart=: a killed ssh comes back on its own -----------------------------
systemctl --user kill --signal=KILL "$unit"
sleep 2
for _ in $(seq 1 60); do
    ssh -o BatchMode=yes "$host" "nc -z 127.0.0.1 $port" 2>/dev/null && break
    sleep 0.5
done
ssh -o BatchMode=yes "$host" "nc -z 127.0.0.1 $port" 2>/dev/null || fail "the tunnel did not come back after its ssh was killed"
code="$(remote_curl good -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/v1/status")"
[ "$code" = 200 ] || fail "status after the restart: HTTP $code"
say "  killed the tunnel's ssh: restarted, status $code again"

# --- teardown proves the port goes away --------------------------------------
systemctl --user stop "$unit"
for _ in $(seq 1 50); do
    ssh -o BatchMode=yes "$host" "nc -z 127.0.0.1 $port" 2>/dev/null || break
    sleep 0.2
done
ssh -o BatchMode=yes "$host" "nc -z 127.0.0.1 $port" 2>/dev/null && fail "the Mac port outlived the tunnel"
say "tunnel stopped; the Mac port is closed"
say "PASS"
