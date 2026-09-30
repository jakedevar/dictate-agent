#!/usr/bin/env bash
# Measure cold start, resident memory and idle cost of the release `dictated`
# in an isolated environment (private XDG dirs, audio-less, no display, no
# notifications) — the numbers recorded in
# thoughts/shared/research/2026-09-29-s03-e2e-latency.md. Run from the repo root
# after `cargo build --release -p dictated -p dictate-cli`. Never touches the
# live daemon and never opens the microphone.
set -euo pipefail
repo=$(pwd)
model="$HOME/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin"
root=$(mktemp -d /tmp/dictated-measure.XXXXXX); mkdir -p $root/run $root/config/dictate-agent $root/data; chmod 700 $root/run
cat > $root/config/dictate-agent/config.toml <<CFG
[whisper]
model = "large-v3-turbo"
model_path = "$model"
language = "en"
[audio]
capture = false
[grammar]
enabled = false
[notifications]
enabled = false
CFG
export XDG_RUNTIME_DIR=$root/run XDG_CONFIG_HOME=$root/config XDG_DATA_HOME=$root/data DICTATE_SOCKET=$root/run/dictate-agent/dictated.sock
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
now() { python3 -c 'import time;print(time.monotonic())'; }
t0=$(now)
target/release/dictated > $root/log 2>&1 &
pid=$!
while [ ! -S $DICTATE_SOCKET ]; do sleep 0.005; done
t_sock=$(now)
until target/release/dictate status --json | python3 -c 'import json,sys; s=json.load(sys.stdin); sys.exit(0 if s["model"]["loaded"] else 1)'; do sleep 0.02; done
t_loaded=$(now)
python3 - <<PY
print(f"socket bound        : {($t_sock-$t0)*1000:.0f} ms after exec")
print(f"model loaded (ready): {($t_loaded-$t0)*1000:.0f} ms after exec")
PY
sleep 2
grep -E 'VmRSS|VmHWM' /proc/$pid/status | tr '\n' ' '; echo "(idle after load)"
nvidia-smi --query-compute-apps=pid,used_memory --format=csv,noheader,nounits | awk -F, -v p=$pid '$1+0==p {print "GPU memory idle      : " $2+0 " MiB"}'
ps -o %cpu=,rss= -p $pid | awk '{print "cpu% since start    : " $1 "  (avg incl. load)"}'
for c in short medium long; do
  target/release/dictate transcribe crates/dictated/tests/fixtures/e2e/$c.wav --privacy 2>&1 >/dev/null | sed "s/^/first $c: /"
done
# The decode+resample cost of realistic uploads (needs sox): the fixtures are
# already 16 kHz mono, so convert them to what a phone or a recorder produces.
if command -v sox >/dev/null; then
  for spec in "44100 2" "48000 2" "48000 1"; do
    set -- $spec
    sox crates/dictated/tests/fixtures/e2e/long.wav -r $1 -c $2 -b 16 $root/up.wav
    printf 'long clip as %s Hz / %s ch (%s KB): ' $1 $2 $(( $(stat -c %s $root/up.wav) / 1024 ))
    target/release/dictate transcribe $root/up.wav --privacy 2>&1 >/dev/null
  done
fi
grep -E 'VmRSS|VmHWM' /proc/$pid/status | tr '\n' ' '; echo "(after transcribing)"
nvidia-smi --query-compute-apps=pid,used_memory --format=csv,noheader,nounits | awk -F, -v p=$pid '$1+0==p {print "GPU memory after   : " $2+0 " MiB"}'
# idle CPU over 10 s
a=$(awk '{print $14+$15}' /proc/$pid/stat); sleep 10; b=$(awk '{print $14+$15}' /proc/$pid/stat)
python3 -c "print(f'idle CPU over 10 s   : {($b-$a)/100/10*100:.2f} % of one core')"
kill -TERM $pid; wait $pid 2>/dev/null || true
rm -rf $root
