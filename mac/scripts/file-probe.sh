#!/bin/bash
# On the Mac: file mode against a stub hub. Synthesizes a short spoken clip
# with `say` (synthetic, no personal audio), launches the bundled app with
# `--transcribe-file` (through LaunchServices, as the end-to-end does) pointed
# at a one-shot stub on an ephemeral 127.0.0.1 port with a throwaway token,
# and checks: the stub saw POST /v1/transcribe?app=FileProbe with the bearer
# token and a 16 kHz mono 16-bit WAV of the right length, and the app printed
# the stub's text in its DICTATEHUB_RESULT line. Never contacts the real hub.
# Run via `mac/scripts/remote.sh file-probe`.
set -euo pipefail
cd "$(dirname "$0")/.."
APP=${1:-build/DictateHub.app}
[[ -d $APP ]] || { echo "no $APP; run bundle first" >&2; exit 1; }

work=build/file-probe
rm -rf "$work"
mkdir -m 700 -p "$work/token"
(umask 077; printf 'probe-token-not-a-secret\n' > "$work/token/api-token")
say -o "$work/clip.wav" --data-format=LEI16@16000 "Synthetic file mode probe, one two three."
afinfo "$work/clip.wav" | grep -E 'Data format|estimated duration' | sed 's/^/clip: /'

port=$((20000 + RANDOM % 20000))
cat > "$work/stub.py" <<'EOF'
import json, sys, http.server
out = sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n)
        with open(out + ".body", "wb") as f:
            f.write(body)
        with open(out + ".head", "w") as f:
            f.write(self.requestline + "\n" + str(self.headers))
        reply = json.dumps({"type": "transcript", "text": "stub transcript for file mode",
                            "route": "type", "word_count": 5, "model": "stub"}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(reply)))
        self.end_headers()
        self.wfile.write(reply)
    def log_message(self, *a):
        pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).handle_request()
EOF
python3 -I "$work/stub.py" "$port" "$PWD/$work/request" &
stub=$!
trap 'kill $stub 2>/dev/null || true' EXIT
sleep 0.7

open -W -n -g --stdout "$work/out.log" --stderr "$work/out.log" "$APP" --args \
    --transcribe-file "$PWD/$work/clip.wav" --app FileProbe \
    --endpoint "http://127.0.0.1:$port" --token-dir "$PWD/$work/token"

echo "--- app output"
cat "$work/out.log"
echo "--- request the stub received"
cat "$work/request.head" 2>/dev/null | tr -d '\r' | sed -n '1,8p'

ok=1
head -1 "$work/request.head" | grep -q '^POST /v1/transcribe?app=FileProbe HTTP/1.1' || { echo "FAIL: no POST /v1/transcribe?app=FileProbe" >&2; ok=0; }
grep -qi '^authorization: Bearer probe-token-not-a-secret' "$work/request.head" || { echo "FAIL: no bearer token" >&2; ok=0; }
grep -qi '^content-type: audio/wav' "$work/request.head" || { echo "FAIL: no audio/wav" >&2; ok=0; }
# The body is the live encoder's WAV: RIFF/WAVE, PCM, mono, 16 kHz, 16-bit,
# and as many samples as the clip (afinfo's frame count).
python3 -I - "$work/request.body" "$work/clip.wav" <<'EOF' || ok=0
import struct, sys, wave
body = open(sys.argv[1], "rb").read()
riff, _, wave_id, fmt, _, tag, ch, rate, _, _, bits, data, n = struct.unpack("<4sI4s4sIHHIIHH4sI", body[:44])
frames = wave.open(sys.argv[2]).getnframes()
ok = (riff, wave_id, fmt, data) == (b"RIFF", b"WAVE", b"fmt ", b"data") and (tag, ch, rate, bits) == (1, 1, 16000, 16) \
    and n == len(body) - 44 and n // 2 == frames
print(f"body: {len(body)} bytes, pcm={tag} ch={ch} rate={rate} bits={bits} samples={n // 2} (clip {frames})")
sys.exit(0 if ok else 1)
EOF
grep -q '^DICTATEHUB_RESULT .*"ok":true' "$work/out.log" || { echo "FAIL: no ok result line" >&2; ok=0; }
grep -q '"text":"stub transcript for file mode"' "$work/out.log" || { echo "FAIL: text not passed through" >&2; ok=0; }
grep -q '"inserted":false' "$work/out.log" || { echo "FAIL: inserted without --insert" >&2; ok=0; }
[[ $ok == 1 ]] && echo "FILE PROBE OK: bundled app sent the clip through the live client and printed the stub's text"
[[ $ok == 1 ]]
