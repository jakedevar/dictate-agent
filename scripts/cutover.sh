#!/usr/bin/env bash
# Operator cutover. Every non-live run requires HOME + all XDG roots isolated.
source "$(dirname "$0")/cutover-common.sh"
operation=cutover
parse_args "$@"
initialize

for executable in "$new_binary" "$cli" "$old_binary"; do
    [ -x "$executable" ] || fail "missing $executable; run just install (retain the old binary)"
done
[ -f "$config_root/systemd/user/dictated.service" ] || fail 'missing dictated.service; run just install-unit'
if [ -e "$dropin" ] && ! owned_dropin; then fail "unowned drop-in: $dropin"; fi

probe=""
probe_pid=""
recover=false
cleanup_probe() {
    if [ -n "$probe_pid" ]; then
        kill -TERM "$probe_pid" 2>/dev/null || true
        python3 "$helper" wait exit "$probe_pid" "$new_binary" || return 1
        wait "$probe_pid" 2>/dev/null || true
        probe_pid=""
    fi
    if [ -n "$probe" ]; then rm -rf -- "$probe"; probe=""; fi
}
finish() {
    local rc=$?
    trap - EXIT INT TERM
    cleanup_probe || rc=1
    if [ "$rc" -ne 0 ] && [ "$recover" = true ]; then
        echo 'Cutover failed; automatically rolling back.' >&2
        rollback || { echo 'Automatic rollback failed; inspect the logs and run cutover-rollback.sh.' >&2; rc=1; }
    fi
    exit "$rc"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

doctor_check() {
    local phase=$1 output=$2 rc=0
    shift 2
    "$cli" doctor "$@" --json >"$output" || rc=$?
    python3 "$helper" doctor "$output" "$rc" "$phase"
}

print_bindings() {
    local pid
    if pid="$(python3 "$helper" pid "$new_pid" "$new_binary")" && [ "$(cat "$legacy_pid" 2>/dev/null)" = "$pid" ]; then
        echo 'Existing scripts/dictate-toggle and scripts/dictate-cancel reach dictated through the legacy PID file.'
    else
        echo 'Legacy PID is not claimed. Set these i3 bindings manually:'
        printf 'bindsym $mod+n exec --no-startup-id %s toggle\n' "$cli"
        printf 'bindsym $mod+Shift+n exec --no-startup-id %s cancel\n' "$cli"
    fi
}

# A re-run verifies the existing service without restarting or taking another
# backup, preserving the original pre-cutover snapshot and one-start import.
if systemctl --user is-active --quiet dictated; then
    python3 "$helper" pid "$new_pid" "$new_binary" >/dev/null || fail 'active service has no verified dictated PID'
    "$new_binary" --check-config
    report="$(mktemp "$state_root/recheck-XXXXXX.json")"
    doctor_check preflight "$report" --quick
    recover=true
    "$cli" status
    doctor_check postflight "$report"
    recover=false
    print_bindings
    echo 'Already running; startup flags are unchanged. Roll back first to change them.'
    exit 0
fi

"$new_binary" --check-config
attempt="$(mktemp -d "$state_root/$(date -u +%Y%m%dT%H%M%S.%N)-XXXXXX")"
python3 "$helper" config "$config_file" "$attempt/steady.toml" steady "$language" false
python3 "$helper" config "$config_file" "$attempt/first-start.toml" steady "$language" "$import_history"
"$new_binary" --check-config --config "$attempt/first-start.toml"

# Doctor requires a running daemon. Use a private control plane, no capture,
# hotkeys or notifications, and private dictionary/history state. The real
# config, old legacy PID and old DB are never used for writes by this probe.
probe="$(mktemp -d /tmp/dc-XXXXXX)"
mkdir -p "$probe/config" "$probe/data" "$probe/run"
python3 "$helper" config "$attempt/steady.toml" "$probe/config.toml" probe "$language" false
XDG_CONFIG_HOME="$probe/config" XDG_DATA_HOME="$probe/data" XDG_RUNTIME_DIR="$probe/run" \
    DICTATE_SOCKET="$probe/run/dictate-agent/dictated.sock" \
    "$new_binary" --config "$probe/config.toml" >"$attempt/preflight-daemon.log" 2>&1 9>&- &
probe_pid=$!
python3 "$helper" wait claim "$probe/run/dictate-agent/dictated.pid" "$new_binary" >/dev/null
DICTATE_SOCKET="$probe/run/dictate-agent/dictated.sock" doctor_check preflight "$attempt/preflight.json" --quick
cleanup_probe

[ ! -f "$config_file" ] || cp -p -- "$config_file" "$attempt/config.toml.pre-cutover"
if [ -f "$legacy_db" ]; then
    python3 "$helper" backup-db "$legacy_db" "$attempt/history.db.pre-cutover"
fi
echo "Pre-cutover backups: $attempt"
old_pid="$(find_old)"
recover=true
if [ -n "$old_pid" ]; then
    python3 "$helper" match "$old_pid" "$old_binary" || fail 'old PID changed during preflight'
    kill -TERM "$old_pid"
    python3 "$helper" wait exit "$old_pid" "$old_binary"
fi
mkdir -p "$(dirname "$dropin")"
python3 "$helper" dropin "$new_binary" "$attempt/first-start.toml" >"$dropin"
systemctl --user daemon-reload
systemctl --user enable --now dictated
python3 "$helper" wait claim "$new_pid" "$new_binary" >/dev/null
"$cli" status
doctor_check postflight "$attempt/postflight.json"
if [ "$claude_dictionary" = true ]; then
    "$cli" dict list --json >"$attempt/dictionary.json"
    if ! python3 "$helper" has-entry "$attempt/dictionary.json" Claude; then
        "$cli" dict add Claude --sounds-like cloud
    fi
fi
# The running process already read its startup config. Future starts use the
# steady copy with import disabled; no shared config edit or extra restart.
python3 "$helper" dropin "$new_binary" "$attempt/steady.toml" >"$dropin"
systemctl --user daemon-reload
recover=false
print_bindings
echo 'Cutover complete. Return with scripts/cutover-rollback.sh (add --live for the real HOME).'
