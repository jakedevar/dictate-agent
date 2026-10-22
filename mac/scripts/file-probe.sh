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

# le FILE OFFSET LEN: little-endian unsigned integer of LEN bytes at OFFSET.
le() {
    local v=0 i b
    local -a bytes
    read -r -a bytes <<< "$(od -An -v -tu1 -j "$2" -N "$3" "$1")"
    for ((i = ${#bytes[@]} - 1; i >= 0; i--)); do v=$((v * 256 + bytes[i])); done
    echo "$v"
}
wav_check() { # request body, clip: RIFF/WAVE/PCM mono 16 kHz 16-bit, sample count == the clip's
    local f=$1 clip=$2 size tag ch rate bits n frames
    size=$(stat -f %z "$f")
    [[ $(head -c 4 "$f") == RIFF && $(dd if="$f" bs=1 skip=8 count=8 2>/dev/null) == 'WAVEfmt ' \
        && $(dd if="$f" bs=1 skip=36 count=4 2>/dev/null) == data ]] || { echo "body: not a RIFF/WAVE fmt/data header" >&2; return 1; }
    tag=$(le "$f" 20 2); ch=$(le "$f" 22 2); rate=$(le "$f" 24 4); bits=$(le "$f" 34 2); n=$(le "$f" 40 4)
    # afinfo prints the clip's PCM byte count as "audio bytes: N".
    frames=$(( $(afinfo "$clip" | sed -n 's/^audio bytes: *//p') / 2 ))
    echo "body: $size bytes, pcm=$tag ch=$ch rate=$rate bits=$bits samples=$((n / 2)) (clip $frames)"
    [[ $tag == 1 && $ch == 1 && $rate == 16000 && $bits == 16 && $n == $((size - 44)) && $((n / 2)) == $frames ]]
}

port=$((20000 + RANDOM % 20000))
swiftc -O -o "$work/stub" scripts/file-probe-stub.swift
"$work/stub" "$port" "$PWD/$work/request" &
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
wav_check "$work/request.body" "$work/clip.wav" || ok=0
grep -q '^DICTATEHUB_RESULT .*"ok":true' "$work/out.log" || { echo "FAIL: no ok result line" >&2; ok=0; }
grep -q '"text":"stub transcript for file mode"' "$work/out.log" || { echo "FAIL: text not passed through" >&2; ok=0; }
grep -q '"inserted":false' "$work/out.log" || { echo "FAIL: inserted without --insert" >&2; ok=0; }
[[ $ok == 1 ]] && echo "FILE PROBE OK: bundled app sent the clip through the live client and printed the stub's text"
[[ $ok == 1 ]]
