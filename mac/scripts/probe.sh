#!/bin/bash
# On the Mac: launch the bundled app in smoke mode against a one-shot nc stub
# on an ephemeral-range 127.0.0.1 port, with a throwaway token, and check that
# its "Test connection" (GET /v1/status) reached the stub over plain HTTP and
# parsed the answer. Proves the *bundled* app (its Info.plist, ATS, signing)
# can talk to the hub endpoint shape. Run via `mac/scripts/remote.sh probe`.
set -euo pipefail
cd "$(dirname "$0")/.."
APP=build/DictateHub.app
[[ -d $APP ]] || { echo "no $APP; run bundle first" >&2; exit 1; }

port=$((20000 + RANDOM % 20000))
work=build/probe
rm -rf "$work"
mkdir -m 700 -p "$work/token"
(umask 077; printf 'probe-token-not-a-secret\n' > "$work/token/api-token")

body='{"type":"status","state":"idle","daemon":{"name":"dictated","version":"0.0.0-stub"},"model":{"name":"stub-model","loaded":true,"backend":"cpu"}}'
printf 'HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: %d\r\nConnection: close\r\n\r\n%s' \
    "${#body}" "$body" > "$work/response"

nc -l 127.0.0.1 "$port" < "$work/response" > "$work/request" 2>/dev/null &
nc_pid=$!
trap 'kill $nc_pid 2>/dev/null || true' EXIT
sleep 0.5

open -n --stdout "$work/launch.log" --stderr "$work/launch.log" "$APP" --args \
    --smoke 4 --probe --endpoint "http://127.0.0.1:$port" --token-dir "$PWD/$work/token"
sleep 7

echo "--- app log"
cat "$work/launch.log"
echo "--- request the stub received (first line + headers)"
tr -d '\r' < "$work/request" | sed -n '1,12p'

ok=1
head -1 "$work/request" | grep -q '^GET /v1/status HTTP/1.1' || { echo "FAIL: stub saw no GET /v1/status" >&2; ok=0; }
grep -qi '^authorization: Bearer probe-token-not-a-secret' "$work/request" || { echo "FAIL: no bearer token" >&2; ok=0; }
grep -q "Hub OK: dictated 0.0.0-stub · stub-model (cpu)" "$work/launch.log" || { echo "FAIL: app did not parse the status" >&2; ok=0; }
[[ $ok == 1 ]] && echo "PROBE OK: bundled app reached the stub at 127.0.0.1:$port and parsed /v1/status"
[[ $ok == 1 ]]
