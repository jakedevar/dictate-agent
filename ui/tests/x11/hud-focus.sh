#!/usr/bin/env bash
# The Flow bar must never take keyboard focus and must let clicks through.
#
# Runs entirely inside a private X server: Xvfb on a display it picks itself
# (-displayfd), i3 inside it, a private D-Bus session (so the tray icon never
# reaches the real desktop's tray), and private XDG directories. The real
# DISPLAY is never used: it is unset before anything starts.
#
# The focused window is `xev`, which logs every key and button event it
# receives — so "focus stayed put" and "the click went through" are observed
# deliveries, not inferred from window properties.
#
# What it proves, over two full show/hide cycles (the second catches hints
# that only hold for the first map):
#   1. the bar maps on `recording` and unmaps after a result lingers;
#   2. X input focus and i3's focused container stay on the target window
#      through every state, including with the pointer resting on the bar and
#      i3's focus_follows_mouse on;
#   3. keystrokes sent while the bar is up reach the target;
#   4. a click at the centre of the bar is delivered to the target underneath;
#   5. i3 floats the bar on its own (no for_window rule in this config), and
#      it carries WM_HINTS input=False and _NET_WM_WINDOW_TYPE_NOTIFICATION;
#   6. it sits bottom-centre on the monitor.
#
# Usage: ui/tests/x11/hud-focus.sh [--shots DIR]
#   DICTATE_UI_BIN  the UI binary   (default ui/src-tauri/target/release/dictate-ui)
#   STUB_BIN        the stub daemon (default ui/src-tauri/target/debug/examples/stub_daemon)
# With --shots, crops of the bar in each state are written to DIR.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ui_dir="$(cd "$here/../.." && pwd)"
UI_BIN="${DICTATE_UI_BIN:-$ui_dir/src-tauri/target/release/dictate-ui}"
STUB_BIN="${STUB_BIN:-$ui_dir/src-tauri/target/debug/examples/stub_daemon}"
SHOTS=""
if [[ "${1:-}" == "--shots" ]]; then
  SHOTS="$(mkdir -p "$2" && cd "$2" && pwd)"
fi

# Re-exec under a private D-Bus session bus, once.
if [[ -z "${HUD_FOCUS_PRIVATE_BUS:-}" ]]; then
  export HUD_FOCUS_PRIVATE_BUS=1
  exec dbus-run-session -- "$0" "$@"
fi

for tool in Xvfb i3 i3-msg xev xdotool xprop xwininfo jq; do
  command -v "$tool" >/dev/null || { echo "SKIP: $tool is not installed" >&2; exit 2; }
done
[[ -x "$UI_BIN" ]] || { echo "FAIL: no UI binary at $UI_BIN (just ui-build)" >&2; exit 1; }
[[ -x "$STUB_BIN" ]] || { echo "FAIL: no stub daemon at $STUB_BIN" >&2; exit 1; }

# Never inherit the real session's display or runtime state.
unset DISPLAY WAYLAND_DISPLAY I3SOCK DICTATE_SOCKET
work="$(mktemp -d "${TMPDIR:-/tmp}/hud-focus.XXXXXX")"
# Unix sockets need a short path; keep the runtime dir under /tmp.
run="$(mktemp -d /tmp/hudf.XXXXXX)"
chmod 700 "$run"
export XDG_RUNTIME_DIR="$run"
export XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data" XDG_CACHE_HOME="$work/cache"
export HOME="$work/home"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$HOME"
export DICTATE_SOCKET="$run/dictated.sock"
# Software rendering: Xvfb has no GPU, and the test must not depend on one.
export WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1

pids=()
cleanup() {
  exec 3>&- 2>/dev/null || true
  for ((i = ${#pids[@]} - 1; i >= 0; i--)); do kill "${pids[i]}" 2>/dev/null || true; done
  sleep 0.3
  for p in "${pids[@]}"; do kill -9 "$p" 2>/dev/null || true; done
  rm -rf "$work" "$run"
}
trap cleanup EXIT

failures=0
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; failures=$((failures + 1)); }
check() { local what="$1"; shift; if "$@"; then pass "$what"; else fail "$what"; fi; }

wait_for() { # seconds, then a command
  local deadline=$((SECONDS + $1)); shift
  until "$@" 2>/dev/null; do
    ((SECONDS < deadline)) || return 1
    sleep 0.1
  done
}

# --- X server and window manager -------------------------------------------
W=1600 H=900
exec 4> "$work/displayfd"
Xvfb -displayfd 4 -screen 0 "${W}x${H}x24" -nolisten tcp -noreset >"$work/xvfb.log" 2>&1 &
pids+=($!)
exec 4>&-
wait_for 10 test -s "$work/displayfd" || { echo "FAIL: Xvfb did not start" >&2; exit 1; }
export DISPLAY=":$(head -n1 "$work/displayfd")"
echo "private display $DISPLAY"

cat >"$work/i3.config" <<'EOF'
# Deliberately no for_window or no_focus rule for the bar: the window's own
# hints must be enough. focus_follows_mouse is on, the harder case.
font pango:monospace 8
focus_follows_mouse yes
default_border pixel 1
EOF
i3 -c "$work/i3.config" >"$work/i3.log" 2>&1 &
pids+=($!)
wait_for 10 i3 --get-socketpath >/dev/null || { echo "FAIL: i3 did not start" >&2; exit 1; }
export I3SOCK="$(i3 --get-socketpath)"
wait_for 10 i3-msg -t get_version >/dev/null

# --- stub daemon and the UI ------------------------------------------------
mkfifo "$work/stub.in"
"$STUB_BIN" "$DICTATE_SOCKET" <"$work/stub.in" >"$work/stub.log" 2>&1 &
pids+=($!)
exec 3> "$work/stub.in"
say() { echo "$*" >&3; }
wait_for 10 test -S "$DICTATE_SOCKET"

"$UI_BIN" >"$work/ui.log" 2>&1 &
pids+=($!)
hud_id() { xdotool search --name '^Flow bar$' 2>/dev/null | head -n1; }
wait_for 30 test -n "$(hud_id)" || { echo "FAIL: the Flow bar window never appeared"; cat "$work/ui.log"; exit 1; }
HUD="$(hud_id)"
wait_for 10 grep -q "client connected" "$work/stub.log" || true
sleep 1 # let the bridge finish its handshake and subscribe

# --- the target: xev, focused, logging every key and button ---------------
xev -1 -name focus-target -event keyboard -event button -event focus >"$work/xev.log" 2>&1 &
pids+=($!)
target_id() { xdotool search --name '^focus-target$' 2>/dev/null | head -n1; }
wait_for 10 test -n "$(target_id)" || { echo "FAIL: xev did not map"; exit 1; }
TARGET="$(target_id)"
wait_for 5 test "$(xdotool getwindowfocus)" = "$TARGET" || true
xdotool mousemove 40 40
hex() { printf '0x%x' "$1"; }
TARGET_HEX="$(hex "$TARGET")"

focused_x() { [[ "$(xdotool getwindowfocus)" == "$TARGET" ]]; }
focused_i3() {
  [[ "$(i3-msg -t get_tree | jq -r '.. | objects | select(.focused? == true) | .window // empty')" == "$TARGET" ]]
}
viewable() { xwininfo -id "$HUD" | grep -q 'Map State: IsViewable'; }
unmapped() { xwininfo -id "$HUD" | grep -q 'Map State: IsUnMapped'; }
keypresses() { grep -c "^KeyPress event.*window $TARGET_HEX" "$work/xev.log" || true; }
buttonpresses() { grep -c "^ButtonPress event.*window $TARGET_HEX" "$work/xev.log" || true; }

check "baseline: the target has X input focus" focused_x
check "baseline: the target is i3's focused container" focused_i3
before=$(keypresses)
xdotool key a
wait_for 3 test "$(keypresses)" -gt "$before" && pass "baseline: keystrokes reach the target" || fail "baseline: keystrokes reach the target"

geometry() { # prints "x y w h" of the bar in root coordinates
  xwininfo -id "$HUD" | awk '/Absolute upper-left X/{x=$4} /Absolute upper-left Y/{y=$4} /^  Width/{w=$2} /^  Height/{h=$2} END{print x, y, w, h}'
}
shot() { # name
  [[ -n "$SHOTS" ]] || return 0
  command -v import >/dev/null || return 0
  read -r gx gy gw gh < <(geometry)
  import -window root -crop "${gw}x${gh}+${gx}+${gy}" +repage "$SHOTS/hud-$1.png" 2>/dev/null || true
}

cycle() { # n, terminal-state
  local n="$1" result="$2"
  echo "--- cycle $n (ends in $result)"
  say "state idle recording"
  if ! wait_for 5 viewable; then fail "cycle $n: the bar maps on recording"; return; fi
  pass "cycle $n: the bar maps on recording"
  say "speak 2.5"
  sleep 0.6
  check "cycle $n: X focus stays on the target while the bar is up" focused_x
  check "cycle $n: i3 focus stays on the target while the bar is up" focused_i3

  # Keys typed now must land in the target.
  local k=$(keypresses)
  xdotool key b
  wait_for 3 test "$(keypresses)" -gt "$k" && pass "cycle $n: keystrokes reach the target while the bar is up" \
    || fail "cycle $n: keystrokes reach the target while the bar is up"

  # The pointer rests on the bar, then clicks its centre.
  read -r gx gy gw gh < <(geometry)
  local cx=$((gx + gw / 2)) cy=$((gy + gh / 2))
  xdotool mousemove "$cx" "$cy"
  sleep 0.3
  check "cycle $n: focus_follows_mouse does not hand the bar focus" focused_x
  local under
  under="$(xdotool getmouselocation --shell | sed -n 's/^WINDOW=//p')"
  check "cycle $n: the window under the pointer at the bar's centre is not the bar" test "$under" != "$HUD"
  local b=$(buttonpresses)
  xdotool click 1
  wait_for 3 test "$(buttonpresses)" -gt "$b" && pass "cycle $n: a click on the bar is delivered to the target underneath" \
    || fail "cycle $n: a click on the bar is delivered to the target underneath"
  if grep "^ButtonPress event.*window $TARGET_HEX" "$work/xev.log" | tail -n1 | grep -q "root:($cx,$cy)"; then
    pass "cycle $n: ...at the bar's centre ($cx,$cy)"
  else
    fail "cycle $n: ...at the bar's centre ($cx,$cy): $(grep '^ButtonPress' "$work/xev.log" | tail -n1)"
  fi
  xdotool mousemove 40 40
  [[ "$n" == 1 ]] && shot recording

  say "state recording transcribing"
  sleep 0.4
  [[ "$n" == 1 ]] && shot transcribing
  check "cycle $n: focus unchanged while transcribing" focused_x
  say "state transcribing formatting"
  sleep 0.2
  check "cycle $n: focus unchanged while formatting" focused_x
  say "state formatting $result"
  sleep 0.3
  [[ "$n" == 1 || "$result" != done ]] && shot "$result"
  check "cycle $n: focus unchanged on $result" focused_x
  if wait_for 4 unmapped; then pass "cycle $n: the bar unmaps after its linger"; else fail "cycle $n: the bar unmaps after its linger"; fi
  check "cycle $n: X focus is still on the target after the bar hides" focused_x
  check "cycle $n: i3 focus is still on the target after the bar hides" focused_i3
}

cycle 1 done

# --- static properties (checked once the bar has been mapped) --------------
hints="$(xprop -id "$HUD" WM_HINTS _NET_WM_WINDOW_TYPE 2>/dev/null)"
check "WM_HINTS says the bar accepts no input focus" grep -q "accepts input or input focus: False" <<<"$hints"
check "the bar is _NET_WM_WINDOW_TYPE_NOTIFICATION" grep -q "_NET_WM_WINDOW_TYPE_NOTIFICATION" <<<"$hints"

say "state idle recording"
wait_for 5 viewable || true
float="$(i3-msg -t get_tree | jq -r --argjson w "$HUD" '.. | objects | select(.window? == $w) | .floating')"
check "i3 floats the bar with no user rule (floating=$float)" test "$float" = auto_on -o "$float" = user_on
read -r gx gy gw gh < <(geometry)
mid=$((gx + gw / 2))
check "the bar is horizontally centred (centre x=$mid of $W)" test "$mid" -ge $((W / 2 - 2)) -a "$mid" -le $((W / 2 + 2))
check "the bar sits near the bottom edge (y=$gy h=$gh of $H)" test $((gy + gh)) -le "$H" -a $((gy + gh)) -ge $((H - 120))
say "state recording cancelled"
wait_for 4 unmapped || true

cycle 2 error

echo
if ((failures == 0)); then
  echo "hud-focus: all checks passed"
else
  echo "hud-focus: $failures check(s) failed"
  echo "--- ui.log";   tail -n 30 "$work/ui.log"
  echo "--- stub.log"; tail -n 10 "$work/stub.log"
  exit 1
fi
