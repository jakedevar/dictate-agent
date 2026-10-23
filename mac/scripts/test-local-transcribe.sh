#!/bin/bash
# Shell contract for the second client. --socket-fixture is run by the Swift
# UDS stub test; standalone mode checks curl argv/stdin without a live daemon.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ ${1:-} == --socket-fixture ]]; then
    DICTATE_LOCAL_DIRECTORY=$2 scripts/local-transcribe.sh "$3"
    exit
fi
tmp=$(mktemp -d "${TMPDIR:-/tmp}/dh-local.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/local/run"
printf 'synthetic-token\n' > "$tmp/local/api-token"
printf 'RIFF-fixture' > "$tmp/clip with spaces.wav"
cat > "$tmp/bin/curl" <<'SH'
#!/bin/bash
set -euo pipefail
printf '%s\n' "$@" > "$FIXTURE_DIR/argv"
cat > "$FIXTURE_DIR/stdin"
printf '{"text":"curl stub"}'
exit "${FIXTURE_EXIT:-0}"
SH
chmod +x "$tmp/bin/curl"
export PATH="$tmp/bin:$PATH" FIXTURE_DIR="$tmp" DICTATE_LOCAL_DIRECTORY="$tmp/local"
pass=0
expect() { if "$@"; then pass=$((pass + 1)); else echo "FAIL: $*" >&2; exit 1; fi; }
scripts/local-transcribe.sh "$tmp/clip with spaces.wav" > "$tmp/out"
expect grep -qF 'Authorization: Bearer synthetic-token' "$tmp/stdin"
expect grep -qxF -- '-K' "$tmp/argv"
expect grep -qxF -- '-' "$tmp/argv"
expect grep -qxF -- "$tmp/local/run/api.sock" "$tmp/argv"
expect grep -qxF -- "@$tmp/clip with spaces.wav" "$tmp/argv"
expect grep -qxF -- 'http://localhost/v1/transcribe' "$tmp/argv"
expect grep -qxF -- 'Host: 127.0.0.1:7313' "$tmp/argv"
expect grep -qxF -- '{"text":"curl stub"}' "$tmp/out"
if grep -q synthetic-token "$tmp/argv"; then echo 'token leaked to argv' >&2; exit 1; fi
pass=$((pass + 1))
if FIXTURE_EXIT=22 scripts/local-transcribe.sh "$tmp/clip with spaces.wav" >/dev/null; then
    echo 'curl failure was hidden' >&2; exit 1
fi
pass=$((pass + 1))
rm "$tmp/argv"
printf 'bad"\nheader = "injected\n' > "$tmp/local/api-token"
if scripts/local-transcribe.sh "$tmp/clip with spaces.wav" > "$tmp/out" 2> "$tmp/err"; then
    echo 'invalid token was accepted' >&2; exit 1
fi
expect test ! -e "$tmp/argv"
rm "$tmp/local/api-token"
if scripts/local-transcribe.sh "$tmp/clip with spaces.wav" >/dev/null 2>/dev/null; then
    echo 'missing token was accepted' >&2; exit 1
fi
expect test ! -e "$tmp/argv"
echo "local-transcribe shell tests: $pass passed, 0 failed"
