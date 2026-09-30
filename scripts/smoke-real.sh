#!/usr/bin/env bash
# Real-hardware smoke test of the release binaries — without a human, without a
# microphone, and without touching the running daily-driver daemon.
#
#   release build -> isolated `dictated` -> `dictate doctor` -> `dictate transcribe`
#   -> assertions -> shut down
#
# ISOLATION (the whole point of this script):
#   * every runtime/config/data path is a private temp directory
#     (XDG_RUNTIME_DIR, XDG_CONFIG_HOME, XDG_DATA_HOME, DICTATE_SOCKET), so the
#     socket, PID files, legacy PID file and history database are all new files;
#   * `[audio] capture = false`: no input device is ever opened (asserted below
#     by inspecting the daemon's open file descriptors);
#   * DISPLAY/WAYLAND_DISPLAY/DBUS_SESSION_BUS_ADDRESS are unset in the daemon,
#     so nothing can be typed and no notification can be shown;
#   * the speech model is only read. If it is not installed the script stops
#     rather than downloading into your models directory;
#   * before/after fingerprints of the live daemon's PID file and history
#     database prove they were not touched.
#
# Usage: scripts/smoke-real.sh          (from anywhere; uses the repo it lives in)
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/cuda/bin:$PATH"
model="${E2E_MODEL:-$HOME/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin}"
fixtures="$repo/crates/dictated/tests/fixtures/e2e"

say()  { printf 'smoke| %s\n' "$*"; }
fail() { printf 'smoke| FAIL: %s\n' "$*" >&2; exit 1; }

[ -f "$model" ] || fail "speech model not installed at $model (run: dictate model pull large-v3-turbo). This script never downloads it."
command -v python3 >/dev/null || fail "python3 is needed for the WER assertion"

# --- fingerprints of what must not change --------------------------------
live_pid_file="$HOME/.config/dictate-agent/dictate.pid"
live_db="$HOME/.local/share/dictate-agent/history.db"
fingerprint() { for f in "$live_pid_file" "$live_db" "$live_db-wal"; do
    if [ -e "$f" ]; then stat -c '%n %s %Y' "$f"; else echo "$f absent"; fi; done; }
# The WAL grows when the live daemon itself logs a dictation while we run; only
# the PID file and the main database file are meaningful for "we did not touch it".
stable_fingerprint() { fingerprint | grep -v -- '-wal'; }
before="$(stable_fingerprint)"

# --- build ---------------------------------------------------------------
say "building release binaries (first build compiles whisper.cpp with CUDA)"
( cd "$repo" && cargo build --release -p dictated -p dictate-cli )
dictated="$repo/target/release/dictated"
dictate="$repo/target/release/dictate"

# --- isolated environment ------------------------------------------------
root="$(mktemp -d /tmp/dictated-smoke.XXXXXX)"
mkdir -p "$root/run" "$root/config/dictate-agent" "$root/data"
chmod 700 "$root/run"
daemon_pid=""
cleanup() {
    if [ -n "$daemon_pid" ] && kill -0 "$daemon_pid" 2>/dev/null; then
        kill -TERM "$daemon_pid" 2>/dev/null || true
        wait "$daemon_pid" 2>/dev/null || true
    fi
    rm -rf "$root"
}
trap cleanup EXIT

cat > "$root/config/dictate-agent/config.toml" <<CFG
[whisper]
model = "large-v3-turbo"
model_path = "$model"
device = "cuda"
language = "en"

[audio]
capture = false
pre_roll_ms = 0

[grammar]
enabled = false

[notifications]
enabled = false
CFG

export XDG_RUNTIME_DIR="$root/run"
export XDG_CONFIG_HOME="$root/config"
export XDG_DATA_HOME="$root/data"
export DICTATE_SOCKET="$root/run/dictate-agent/dictated.sock"
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS

# --- start ---------------------------------------------------------------
say "starting an isolated dictated ($root)"
started=$(date +%s.%N)
RUST_LOG=info "$dictated" > "$root/dictated.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 600); do
    [ -S "$DICTATE_SOCKET" ] && break
    kill -0 "$daemon_pid" 2>/dev/null || { cat "$root/dictated.log" >&2; fail "dictated exited during startup"; }
    sleep 0.1
done
[ -S "$DICTATE_SOCKET" ] || fail "dictated did not bind its socket"
say "socket up after $(python3 -c "print(round($(date +%s.%N)-$started,2))")s"

# --- doctor --------------------------------------------------------------
say "dictate doctor"
if ! "$dictate" doctor | tee "$root/doctor.txt"; then
    fail "doctor reported a failure (see above)"
fi
grep -q 'GPU memory' "$root/doctor.txt" || fail "doctor did not confirm the GPU is in use"
grep -q 'verified against the pinned SHA-256' "$root/doctor.txt" || fail "doctor did not verify the model"

# --- transcribe ----------------------------------------------------------
wer() { python3 - "$1" "$2" <<'PY'
import re, sys
norm = lambda t: re.sub(r"[^a-z0-9' ]+", " ", t.lower()).split()
ref, hyp = norm(open(sys.argv[1]).read()), norm(sys.argv[2])
d = list(range(len(hyp) + 1))
for i, r in enumerate(ref, 1):
    nd = [i]
    for j, h in enumerate(hyp, 1):
        nd.append(min(d[j - 1] + (r != h), d[j] + 1, nd[j - 1] + 1))
    d = nd
print(f"{d[-1] / max(len(ref), 1):.3f}")
PY
}
for clip in short medium long; do
    say "dictate transcribe $clip.wav"
    text="$("$dictate" transcribe "$fixtures/$clip.wav" --privacy 2> "$root/$clip.summary")"
    score="$(wer "$fixtures/$clip.txt" "$text")"
    say "  WER $score   $(cat "$root/$clip.summary")"
    python3 -c "import sys; sys.exit(0 if float('$score') <= 0.10 else 1)" \
        || fail "$clip: WER $score exceeds 0.10 (heard: $text)"
done

# --- no microphone, no typing --------------------------------------------
"$dictate" status --json > "$root/status.json"
python3 - "$root/status.json" <<'PY' || fail "status did not show CUDA + audio-less mode"
import json, sys
s = json.load(open(sys.argv[1]))
assert s["model"]["backend"] == "cuda", s["model"]
assert s["audio"]["capture_enabled"] is False and s["audio"]["input_open"] is False, s["audio"]
PY
if "$dictate" toggle > "$root/toggle.out" 2>&1; then
    fail "toggle succeeded in audio-less mode — something opened a recording session"
fi
grep -q 'capture = false' "$root/toggle.out" || fail "toggle's refusal did not explain audio-less mode: $(cat "$root/toggle.out")"
if ls -l /proc/"$daemon_pid"/fd 2>/dev/null | grep -q '/dev/snd'; then
    fail "the daemon holds an audio device open in audio-less mode"
fi
say "audio-less mode confirmed: no /dev/snd descriptors, toggle refused"

# --- shut down -----------------------------------------------------------
kill -TERM "$daemon_pid"
wait "$daemon_pid" 2>/dev/null || true
daemon_pid=""
[ ! -e "$DICTATE_SOCKET" ] || fail "the socket was left behind after shutdown"

after="$(stable_fingerprint)"
[ "$before" = "$after" ] || { printf 'before:\n%s\nafter:\n%s\n' "$before" "$after" >&2; fail "the live daemon's PID file or history database changed"; }
say "live daemon's PID file and history database untouched"
say "PASS"
