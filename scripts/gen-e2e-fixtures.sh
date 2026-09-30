#!/usr/bin/env bash
# Regenerate the synthetic speech fixtures used by the real-hardware E2E test.
#
# Each `crates/dictated/tests/fixtures/e2e/<name>.txt` is synthesised with
# espeak-ng and converted to 16 kHz mono 16-bit PCM (the pipeline's native rate)
# as `<name>.wav`. The text files are the ground truth the test scores WER
# against; they contain only invented, non-personal sentences, because this
# repository is public.
#
# LIMITATION: espeak-ng is a formant synthesiser. Its voice is robotic, has no
# background noise, no accent variation, and no natural disfluency, so a low WER
# here proves the daemon's plumbing and the model's basic health — NOT real-world
# accuracy. Accuracy work uses local, uncommitted recordings.
#
# Requires: espeak-ng, sox. Output is deterministic for a given espeak-ng build.
set -euo pipefail

dir="$(cd "$(dirname "$0")/.." && pwd)/crates/dictated/tests/fixtures/e2e"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

for txt in "$dir"/*.txt; do
    name="$(basename "$txt" .txt)"
    espeak-ng -v en-us -s 165 -w "$tmp/$name.raw.wav" -f "$txt"
    sox "$tmp/$name.raw.wav" -r 16000 -c 1 -b 16 "$dir/$name.wav"
    printf '%s: %s bytes, %s s\n' "$name" \
        "$(stat -c %s "$dir/$name.wav")" "$(soxi -D "$dir/$name.wav")"
done
