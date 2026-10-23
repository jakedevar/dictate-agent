#!/bin/bash
# Live installed LaunchAgent test. Uses only a synthetic repository WAV;
# bearer credentials go to curl's stdin, never its process arguments.
set -euo pipefail
DATA="$HOME/Library/Application Support/dictated"
LABEL=dev.jakedevar.dictated
HERE=$(cd "$(dirname "$0")/../.." && pwd)
WAV="$HERE/crates/dictated/tests/fixtures/e2e/short.wav"
OUT="$HERE/build/dictated-smoke"
mkdir -p "$OUT"
token=$(cat "$DATA/api-token")
request() {
    printf 'header = "Authorization: Bearer %s"\n' "$token" |
        curl --silent --show-error --max-time 180 --unix-socket "$DATA/run/api.sock" -K - "$@"
}
code=$(curl --silent --unix-socket "$DATA/run/api.sock" -o /dev/null -w '%{http_code}' http://127.0.0.1:7313/v1/status)
[[ $code == 401 ]] || { echo "unauthenticated status: $code" >&2; exit 1; }
echo 'unauthenticated status=401'
request --fail-with-body -H 'Content-Type: audio/wav' --data-binary "@$WAV" \
    -w '\ncold_latency=%{time_total}s http=%{http_code}\n' http://127.0.0.1:7313/v1/transcribe > "$OUT/cold.txt"
cat "$OUT/cold.txt"
grep -q '"text":' "$OUT/cold.txt"
request --fail-with-body -H 'Content-Type: audio/wav' --data-binary "@$WAV" \
    -w '\nwarm_latency=%{time_total}s http=%{http_code}\n' http://127.0.0.1:7313/v1/transcribe > "$OUT/warm.txt"
cat "$OUT/warm.txt"
for pair in 'Origin: http://evil.example|403' 'Host: evil.example|421'; do
    header=${pair%|*}; want=${pair##*|}
    code=$(request -H "$header" -o /dev/null -w '%{http_code}' http://127.0.0.1:7313/v1/status)
    [[ $code == "$want" ]] || { echo "admission test: $code != $want" >&2; exit 1; }
    echo "admission refusal=$code"
done
"$DATA/bin/dictated" --config "$DATA/config.toml" --rotate-api-token >/dev/null
code=$(request -o /dev/null -w '%{http_code}' http://127.0.0.1:7313/v1/status)
[[ $code == 401 ]]
echo 'rotated old token status=401'
token=$(cat "$DATA/api-token")
request --fail-with-body http://127.0.0.1:7313/v1/status > "$OUT/status.json"
cat "$OUT/status.json"
echo
pid=$(launchctl print "gui/$(id -u)/$LABEL" | awk '/pid =/ {print $3; exit}')
[[ $pid =~ ^[0-9]+$ ]]
if lsof -a -p "$pid" -iTCP -sTCP:LISTEN > "$OUT/tcp.txt"; then
    cat "$OUT/tcp.txt"; echo 'unexpected TCP listener' >&2; exit 1
fi
echo "LaunchAgent pid=$pid TCP listeners=0"
stat -f '%Lp %N' "$DATA/run" "$DATA/run/api.sock" "$DATA/api-token"
echo 'LIVE UDS SMOKE PASS'
