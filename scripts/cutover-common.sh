#!/usr/bin/env bash
# Sourced by the two entry points. No actions before argument parsing + guard.
set -Eeuo pipefail
umask 077
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
helper="$script_dir/cutover-support.py"
live=false
restore_config=false
import_history=false
language=""
claude_dictionary=false

fail() { printf 'cutover: %s\n' "$*" >&2; exit 1; }
# dictated writes its PID file a little before it binds the control socket
# (about 0.1 s on the real host), so a PID claim does not mean "answering".
# Retry `dictate status` until it succeeds or the deadline passes.
wait_ready() {
    local deadline=$((SECONDS + ${CUTOVER_READY_SECS:-20}))
    until "$cli" status >/dev/null 2>&1; do
        [ "$SECONDS" -lt "$deadline" ] || return 1
        sleep 0.2
    done
}
parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --live) live=true; shift ;;
            --restore-config)
                [ "$operation" = rollback ] || fail '--restore-config is rollback-only'
                restore_config=true; shift ;;
            --import-history|--claude-dictionary|--language)
                [ "$operation" = cutover ] || fail "$1 is cutover-only"
                case "$1" in
                    --import-history) import_history=true; shift ;;
                    --claude-dictionary) claude_dictionary=true; shift ;;
                    --language)
                        [ "${2:-}" = en ] || fail '--language currently accepts only en'
                        language=en; shift 2 ;;
                esac ;;
            --help|-h)
                echo 'cutover.sh [--live] [--import-history] [--language en] [--claude-dictionary]'
                echo 'cutover-rollback.sh [--live] [--restore-config]'
                exit 0 ;;
            *) fail "unknown argument: $1" ;;
        esac
    done
}

initialize() {
    command -v python3 >/dev/null || fail 'Python 3.11+ is required'
    python3 "$helper" guard "$live"
    config_root="${XDG_CONFIG_HOME:-$HOME/.config}"
    data_root="${XDG_DATA_HOME:-$HOME/.local/share}"
    runtime_root="${XDG_RUNTIME_DIR:-/tmp/dictate-agent-$(id -u)}"
    # Helpers and config copies use the same explicit roots as the daemon.
    export XDG_CONFIG_HOME="$config_root" XDG_DATA_HOME="$data_root" XDG_RUNTIME_DIR="$runtime_root"
    config_file="$config_root/dictate-agent/config.toml"
    legacy_pid="$config_root/dictate-agent/dictate.pid"
    new_pid="$runtime_root/dictate-agent/dictated.pid"
    legacy_db="$data_root/dictate-agent/history.db"
    state_root="$config_root/dictate-agent/cutover"
    dropin="$config_root/systemd/user/dictated.service.d/90-dictate-cutover.conf"
    old_binary="$HOME/.local/bin/dictate-agent"
    new_binary="$HOME/.local/bin/dictated"
    cli="$HOME/.local/bin/dictate"
    mkdir -p "$state_root"
    exec 9>"$state_root/lock"
    flock -n 9 || fail 'another cutover or rollback is running'
}

owned_dropin() {
    [ -f "$dropin" ] && [ "$(head -n 1 "$dropin")" = '# Managed by dictate cutover; remove with cutover-rollback.sh' ]
}

find_old() {
    local pid candidate
    if pid="$(python3 "$helper" pid "$legacy_pid" "$old_binary")"; then
        echo "$pid"; return
    fi
    # pgrep is a discovery aid only. Verify UID, executable/argv and HOME before
    # delivering any signal (also rejects corrupt, zero and reused PID files).
    local -a matches=()
    while read -r candidate; do
        if python3 "$helper" match "$candidate" "$old_binary"; then
            matches+=("$candidate")
        fi
    done < <(pgrep -u "$(id -u)" -f 'dictate-agent' || true)
    [ "${#matches[@]}" -le 1 ] || fail 'multiple old daemons found; refusing to guess'
    if [ "${#matches[@]}" -eq 1 ]; then echo "${matches[0]}"; fi
}

rollback() {
    local pid backup log
    [ -x "$old_binary" ] || fail 'old binary is missing; cannot restart scripts/run.sh'
    if [ -e "$dropin" ] && ! owned_dropin; then
        fail "refusing to remove an unowned drop-in: $dropin"
    fi
    systemctl --user disable --now dictated || return 1
    if owned_dropin; then
        rm -- "$dropin"
        systemctl --user daemon-reload || return 1
    fi
    if [ "$restore_config" = true ]; then
        backup="$(find "$state_root" -mindepth 2 -maxdepth 2 -name config.toml.pre-cutover -print | LC_ALL=C sort | tail -n 1)"
        [ -n "$backup" ] || fail 'no pre-cutover config backup exists'
        if ! cmp -s "$backup" "$config_file"; then
            if [ -e "$config_file" ]; then
                log="$(mktemp -d "$state_root/restore-$(date -u +%Y%m%dT%H%M%S)-XXXXXX")"
                cp -p -- "$config_file" "$log/config.toml.before-restore"
            fi
            cp -p -- "$backup" "$config_file"
        fi
    fi
    pid="$(find_old)"
    if [ -z "$pid" ]; then
        log="$(mktemp "$state_root/rollback-$(date -u +%Y%m%dT%H%M%S)-XXXXXX.log")"
        # Close the lock in the child. It must survive our exit without owning it.
        setsid "$script_dir/run.sh" </dev/null >"$log" 2>&1 9>&- &
    fi
    python3 "$helper" wait claim "$legacy_pid" "$old_binary" >/dev/null || return 1
    echo 'Rollback complete: scripts/run.sh owns the legacy PID file.'
}
