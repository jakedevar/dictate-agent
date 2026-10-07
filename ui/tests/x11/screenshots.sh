#!/usr/bin/env bash
# Screenshots of the hub against a real, isolated `dictated`, with synthetic
# data only, written to ui/docs/screenshots/.
#
# ISOLATION (same rules as scripts/smoke-real.sh):
#   * a private Xvfb (-displayfd) with i3, a private D-Bus session, and the
#     real DISPLAY unset — nothing appears on the desktop, no tray icon reaches
#     the real tray;
#   * every runtime/config/data path is a temp directory, so the daemon's
#     socket, PID files and history database are new files;
#   * `[audio] capture = false`: no input device is opened; the history is
#     filled by uploading the synthetic WAV fixtures with `dictate transcribe`;
#   * the speech model is only read, through a symlink in the temp directory
#     (so no home-directory path appears in a screenshot);
#   * `ollama` on PATH is a stub that fails, so the daemon's "start Ollama"
#     fallback cannot launch a real server;
#   * every Ollama host is a closed local port, so the Doctor and Home pages
#     show what a broken formatter looks like without querying the real
#     Ollama (whose model list would otherwise appear in a screenshot).
#
# Usage: ui/tests/x11/screenshots.sh [OUT_DIR]   (default ui/docs/screenshots)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ui_dir="$(cd "$here/../.." && pwd)"
repo="$(cd "$ui_dir/.." && pwd)"
target="${CARGO_TARGET_DIR:-$repo/target}"
out="$(mkdir -p "${1:-$ui_dir/docs/screenshots}" && cd "${1:-$ui_dir/docs/screenshots}" && pwd)"
UI_BIN="${DICTATE_UI_BIN:-$target/release/dictate-ui}"
profile=release
[[ -x "$target/release/dictated" ]] || profile=debug
DICTATED="${DICTATED_BIN:-$target/$profile/dictated}"
DICTATE="${DICTATE_BIN:-$target/$profile/dictate}"
model="${E2E_MODEL:-$HOME/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin}"
fixtures="$repo/crates/dictated/tests/fixtures/e2e"

# Private everything, set up *before* the private D-Bus session starts so the
# bus and anything it activates inherit it (never the real XDG_RUNTIME_DIR,
# HOME or DISPLAY).
if [[ -z "${SHOTS_STAGE2:-}" ]]; then
  for f in "$UI_BIN" "$DICTATED" "$DICTATE"; do [[ -x "$f" ]] || { echo "missing $f" >&2; exit 1; }; done
  [[ -f "$model" ]] || { echo "speech model not installed at $model; this script never downloads it" >&2; exit 2; }
  for tool in Xvfb i3 xwininfo import dbus-run-session; do command -v "$tool" >/dev/null || { echo "SKIP: no $tool" >&2; exit 2; }; done
  unset DISPLAY WAYLAND_DISPLAY I3SOCK DICTATE_SOCKET DBUS_SESSION_BUS_ADDRESS
  root="$(mktemp -d /tmp/dui-shots.XXXXXX)"
  mkdir -p "$root/run" "$root/config/dictate-agent" "$root/data" "$root/cache" "$root/home" "$root/models"
  chmod 700 "$root/run"
  ln -s "$model" "$root/models/ggml-large-v3-turbo.bin"
  # The daemon's local executor tries `ollama serve` when its host does not
  # answer; make sure that can never start a real server from here.
  mkdir -p "$root/bin"
  printf '#!/bin/sh\necho "ollama disabled for screenshots" >&2\nexit 1\n' >"$root/bin/ollama"
  chmod +x "$root/bin/ollama"
  export PATH="$root/bin:$PATH" OLLAMA_HOST=127.0.0.1:9
  export SHOTS_STAGE2=1 SHOTS_ROOT="$root" SHOTS_OUT="$out"
  export XDG_RUNTIME_DIR="$root/run" XDG_CONFIG_HOME="$root/config" XDG_DATA_HOME="$root/data"
  export XDG_CACHE_HOME="$root/cache" HOME="$root/home"
  export DICTATE_SOCKET="$root/run/dictate-agent/dictated.sock"
  export GTK_USE_PORTAL=0 GIO_USE_VFS=local NO_AT_BRIDGE=1
  export WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1
  status=0
  dbus-run-session -- "$0" || status=$?
  # GTK may have started a private xdg-document-portal, whose FUSE mount in
  # the private runtime dir can outlive the bus by a moment.
  for _ in 1 2 3 4 5; do
    fusermount3 -u "$XDG_RUNTIME_DIR/doc" 2>/dev/null || true
    rm -rf "$XDG_RUNTIME_DIR" 2>/dev/null && break
    sleep 0.5
  done
  rm -rf "$root"
  exit "$status"
fi
root="$SHOTS_ROOT"
out="$SHOTS_OUT"

pids=()
cleanup() {
  for ((i = ${#pids[@]} - 1; i >= 0; i--)); do kill "${pids[i]}" 2>/dev/null || true; done
  sleep 0.5
  for p in "${pids[@]}"; do kill -9 "$p" 2>/dev/null || true; done
}
trap cleanup EXIT
wait_for() { local deadline=$((SECONDS + $1)); shift; until eval "$*" 2>/dev/null; do ((SECONDS < deadline)) || return 1; sleep 0.2; done; }

cat >"$root/config/dictate-agent/config.toml" <<CFG
# Synthetic configuration for the S32 screenshots.

[whisper]
model = "large-v3-turbo"
model_path = "$root/models/ggml-large-v3-turbo.bin"
device = "cuda"
language = "en"

[audio]
capture = false
pre_roll_ms = 0

# The legacy [grammar] spelling, as most existing files still have it. Every
# Ollama host is a closed local port: the pages show a degraded formatter, and
# nothing about the machine's real models can reach a screenshot.
[grammar]
enabled = true
host = "http://127.0.0.1:9"
model = "example-formatter:4b"

[local]
host = "http://127.0.0.1:9"
model = "example-local:4b"

[notifications]
enabled = false
CFG

# --- daemon and synthetic history -------------------------------------------
RUST_LOG=warn "$DICTATED" >"$root/dictated.log" 2>&1 &
pids+=($!)
wait_for 120 test -S "$DICTATE_SOCKET" || { cat "$root/dictated.log" >&2; exit 1; }
for clip in short medium long; do
  "$DICTATE" transcribe "$fixtures/$clip.wav" >/dev/null 2>&1
done
"$DICTATE" transcribe "$fixtures/short.wav" --privacy >/dev/null 2>&1
"$DICTATE" dict add Kubernetes --sounds-like "cube ernetties, kubernetties" >/dev/null
"$DICTATE" dict add Tauri --sounds-like "tory, towery" >/dev/null
"$DICTATE" dict add "Preact" --sounds-like "pre act" >/dev/null
"$DICTATE" dict add "ExampleCorp" --app google-chrome >/dev/null
"$DICTATE" dict disable "ExampleCorp" >/dev/null 2>&1 || true

# --- display --------------------------------------------------------------
exec 4>"$root/displayfd"
Xvfb -displayfd 4 -screen 0 1280x800x24 -nolisten tcp -noreset >"$root/xvfb.log" 2>&1 &
pids+=($!)
exec 4>&-
wait_for 10 test -s "$root/displayfd"
export DISPLAY=":$(head -n1 "$root/displayfd")"
printf 'font pango:sans 9\ndefault_border none\n' >"$root/i3.config"
i3 -c "$root/i3.config" >"$root/i3.log" 2>&1 &
pids+=($!)
wait_for 10 i3 --get-socketpath >/dev/null

capture() { # page, file, extra env...
  local page="$1" file="$2"; shift 2
  env "$@" "$UI_BIN" --hub "$page" >>"$root/ui.log" 2>&1 &
  local pid=$!
  wait_for 30 'xwininfo -root -tree | grep -q "\"dictate\": "' || { echo "hub did not open for $page" >&2; tail "$root/ui.log" >&2; }
  sleep "${SETTLE:-4}" # connect, load, and (Doctor) run the quick checks
  import -window root "$out/$file"
  echo "wrote $out/$file"
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  sleep 0.5
}

capture home hub-home.png
capture history hub-history.png
capture dictionary hub-dictionary.png
capture settings hub-settings.png
SETTLE=8 capture doctor hub-doctor.png
capture home hub-home-dark.png GTK_THEME=Adwaita:dark
