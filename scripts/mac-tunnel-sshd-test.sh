#!/usr/bin/env bash
# scripts/dictate-mac-tunnel against a REAL, isolated OpenSSH server
# (`just mac-tunnel-sshd-test`), the counterpart of the stubbed
# scripts/test-mac-hub.sh. Plays "the Mac" with an sshd run as you, from a
# temp directory: its own config, host key, client key, known_hosts and
# ephemeral port, listening on 127.0.0.1 only. Never touches ~/.ssh, the real
# sshd, the real Mac or any firewall, and never uses an API token.
#
#   a. GatewayPorts no            -> tunnel up, 127.0.0.1 only, traffic flows,
#                                    SIGTERM stops it and the port closes
#   b. GatewayPorts yes           -> refused (78) before any forward
#   c. Match User <you> + yes in an Include'd file (sshd -G alone says "no")
#                                 -> refused before any forward
#   d. an unreadable Include      -> refused before any forward
#   e. sshd really runs GatewayPorts yes, but the preflight is shown a safe
#      decoy (a config changed behind its back) -> the forward really binds
#      the wildcard, and the post-start lsof check kills it (78). This case
#      runs in a private network namespace (unshare), so the wildcard
#      listener is reachable from nowhere but that namespace.
#
# Needs: sshd (absolute path), ssh, ssh-keygen, lsof, python3, unshare (for e;
# skipped if user namespaces are unavailable).
set -uo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
wrapper="$repo/scripts/dictate-mac-tunnel"

sshd_bin=""
for s in /usr/sbin/sshd /usr/bin/sshd; do [ -x "$s" ] && { sshd_bin="$s"; break; }; done
for tool in ssh ssh-keygen lsof python3; do
    command -v "$tool" >/dev/null || { echo "mac-sshd-test: needs $tool" >&2; exit 2; }
done
[ -n "$sshd_bin" ] || { echo "mac-sshd-test: needs sshd" >&2; exit 2; }

pass=0
failures=0
ok() { pass=$((pass + 1)); printf 'ok   %s\n' "$*"; }
bad() { failures=$((failures + 1)); printf 'FAIL %s\n' "$*" >&2; }

free_port() { python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }
listening() { # port -> the local addresses listening on it, one per line
    lsof -nP -iTCP:"$1" -sTCP:LISTEN -Fn 2>/dev/null | sed -n 's/^n//p' | sort -u
}

# setup DIR: keys, client config, a target "hub API" HTTP server.
setup() {
    t="$1"
    mkdir -p "$t/etc/sshd_config.d"
    chmod 700 "$t"
    ssh-keygen -q -t ed25519 -N '' -f "$t/hostkey" -C dmt-host
    ssh-keygen -q -t ed25519 -N '' -f "$t/id" -C dmt-client
    cp "$t/id.pub" "$t/authorized_keys"
    sport="$(free_port)"; fport="$(free_port)"; hport="$(free_port)"
    printf '[127.0.0.1]:%s %s\n' "$sport" "$(cut -d' ' -f1,2 "$t/hostkey.pub")" > "$t/known_hosts"
    cat > "$t/ssh_config" <<CFG
Host dmt-sshd-test
    HostName 127.0.0.1
    Port $sport
    User $(id -un)
    IdentityFile $t/id
    IdentitiesOnly yes
    IdentityAgent none
    UserKnownHostsFile $t/known_hosts
    GlobalKnownHostsFile /dev/null
    LogLevel ERROR
CFG
    printf '#!/bin/sh\nexec ssh -F %s "$@"\n' "$t/ssh_config" > "$t/ssh"
    chmod +x "$t/ssh"
    mkdir -p "$t/www"; echo "hub-api-stand-in" > "$t/www/index.html"
    python3 -I -m http.server --bind 127.0.0.1 --directory "$t/www" "$hport" >/dev/null 2>&1 &
    http_pid=$!
}

# write_cfg FILE GATEWAYPORTS: the sshd config "the Mac" runs.
write_cfg() {
    cat > "$1" <<CFG
Port $sport
ListenAddress 127.0.0.1
HostKey $t/hostkey
PidFile $t/sshd.pid
AuthorizedKeysFile $t/authorized_keys
StrictModes no
UsePAM no
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
AllowTcpForwarding yes
GatewayPorts $2
Include $t/etc/sshd_config.d/*
CFG
}

sshd_pid=""
start_sshd() { # config
    stop_sshd
    "$sshd_bin" -t -f "$1" || { bad "sshd rejects the test config $1"; return 1; }
    "$sshd_bin" -D -e -f "$1" 2>>"$t/sshd.log" &
    sshd_pid=$!
    local i
    for i in $(seq 1 50); do
        [ -n "$(listening "$sport")" ] && return 0
        sleep 0.1
    done
    bad "sshd did not start: $(tail -5 "$t/sshd.log")"
    return 1
}
stop_sshd() {
    if [ -n "$sshd_pid" ]; then kill "$sshd_pid" 2>/dev/null; wait "$sshd_pid" 2>/dev/null; fi
    sshd_pid=""
}

wrapper_env() { # config the preflight reads
    echo DICTATE_MAC_SSH="$t/ssh" DICTATE_MAC_HOST=dmt-sshd-test DICTATE_MAC_SSHD="$sshd_bin" \
        DICTATE_MAC_SSHD_CONFIG="$1" DICTATE_MAC_PORT="$fport" DICTATE_HUB_API_PORT="$hport" \
        DICTATE_MAC_VERIFY_SECS=5
}

# refused NAME PREFLIGHT_CONFIG PATTERN: --check and the full run both exit 78
# and the forward port never listens.
refused() {
    local name="$1" cfg="$2" pat="$3" out rc
    # shellcheck disable=SC2046
    out="$(env $(wrapper_env "$cfg") "$wrapper" --check 2>&1)"; rc=$?
    [ "$rc" = 78 ] && grep -qE -- "$pat" <<<"$out" || { bad "$name: --check rc=$rc: $out"; return; }
    # shellcheck disable=SC2046
    out="$(env $(wrapper_env "$cfg") "$wrapper" 2>&1)"; rc=$?
    [ "$rc" = 78 ] && grep -qE -- "$pat" <<<"$out" || { bad "$name: run rc=$rc: $out"; return; }
    grep -q "tunnel up" <<<"$out" && { bad "$name: a tunnel came up"; return; }
    [ -z "$(listening "$fport")" ] || { bad "$name: port $fport listens: $(listening "$fport")"; return; }
    ok "$name: refused (78), nothing ever listened on $fport"
}

# --- case e, inside the private network namespace ----------------------------
if [ "${1:-}" = --netns-case ]; then
    # Here: a fresh netns (only lo, brought up by the caller) and a user
    # namespace mapping you to yourself. 0.0.0.0 reaches nothing outside.
    t="$2"; sport="$3"; fport="$4"; hport="$5"
    sed -i "s/^Port .*/Port $sport/" "$t/run-yes.conf" "$t/decoy.conf"
    printf '[127.0.0.1]:%s %s\n' "$sport" "$(cut -d' ' -f1,2 "$t/hostkey.pub")" > "$t/known_hosts"
    sed -i "s/^    Port .*/    Port $sport/" "$t/ssh_config"
    python3 -I -m http.server --bind 127.0.0.1 --directory "$t/www" "$hport" >/dev/null 2>&1 &
    http_pid=$!
    trap 'kill $http_pid 2>/dev/null; stop_sshd' EXIT
    start_sshd "$t/run-yes.conf" || exit 1
    # shellcheck disable=SC2046
    out="$(env $(wrapper_env "$t/decoy.conf") "$wrapper" 2>&1)"; rc=$?
    printf '%s\n' "$out" > "$t/netns.out"
    if [ "$rc" = 78 ] && grep -q "preflight ok" <<<"$out" \
        && grep -qE "beyond its loopback \((\*|0\.0\.0\.0):$fport" <<<"$out" && grep -q "tunnel killed" <<<"$out" \
        && [ -z "$(listening "$fport")" ]; then
        echo "NETNS-OK $(grep -o 'listens on port [0-9]* beyond its loopback ([^)]*)' <<<"$out")"
    else
        echo "NETNS-FAIL rc=$rc listening=$(listening "$fport" | tr '\n' ' ')"
    fi
    exit 0
fi

root="$(mktemp -d /tmp/dmt-sshd.XXXXXX)"
http_pid=""
cleanup() {
    stop_sshd
    [ -n "$http_pid" ] && kill "$http_pid" 2>/dev/null
    chmod -R u+rwx "$root" 2>/dev/null
    rm -rf "$root"
}
trap cleanup EXIT
setup "$root"
cfg="$t/sshd_config"

# --- a. GatewayPorts no: up, loopback only, traffic flows, clean stop --------
write_cfg "$cfg" no
if start_sshd "$cfg"; then
    # shellcheck disable=SC2046
    env $(wrapper_env "$cfg") "$wrapper" > "$t/a.log" 2>&1 &
    wpid=$!
    for _ in $(seq 1 100); do grep -q "tunnel up" "$t/a.log" && break; kill -0 "$wpid" 2>/dev/null || break; sleep 0.1; done
    if grep -q "tunnel up" "$t/a.log"; then
        l="$(listening "$fport" | tr '\n' ' ')"
        [ "$l" = "127.0.0.1:$fport " ] && ok "a. GatewayPorts no: up, listening on 127.0.0.1:$fport only" \
            || bad "a. listeners: '$l'"
        body="$(python3 -I -c 'import sys,urllib.request; print(urllib.request.urlopen("http://127.0.0.1:%s/" % sys.argv[1], timeout=5).read().decode().strip())' "$fport" 2>&1)"
        [ "$body" = hub-api-stand-in ] && ok "a. a request through the forward reaches the hub side" || bad "a. through the tunnel: $body"
        kill -TERM "$wpid"; wait "$wpid"; rc=$?
        for _ in $(seq 1 50); do [ -z "$(listening "$fport")" ] && break; sleep 0.1; done
        [ "$rc" = 0 ] && [ -z "$(listening "$fport")" ] && ok "a. SIGTERM: exit 0, the forward port closed" \
            || bad "a. stop: rc=$rc listening=$(listening "$fport")"
    else
        bad "a. the tunnel did not come up: $(cat "$t/a.log")"
        kill "$wpid" 2>/dev/null
    fi
fi

# --- b. GatewayPorts yes ------------------------------------------------------
write_cfg "$cfg" yes
start_sshd "$cfg" && refused "b. GatewayPorts yes" "$cfg" 'effective GatewayPorts is "yes"'

# --- c. a Match block in an Include'd file ------------------------------------
write_cfg "$cfg" no
printf 'Match User %s\n    GatewayPorts yes\n' "$(id -un)" > "$t/etc/sshd_config.d/50-match.conf"
g="$("$sshd_bin" -G -f "$cfg" 2>/dev/null | grep -i "^gatewayports" | tr "[:upper:]" "[:lower:]")"
[ "$g" = "gatewayports no" ] && ok "c. (sshd -G without -C reports '$g': the file scan is what catches it)" \
    || bad "c. expected sshd -G to miss the Match block, got '$g'"
start_sshd "$cfg" && refused "c. Match User $(id -un) + GatewayPorts yes, in an Include" "$cfg" 'inside "Match User'
rm -f "$t/etc/sshd_config.d/50-match.conf"

# --- d. an unreadable Include (the preflight's copy; sshd runs the safe one) --
write_cfg "$cfg" no
mkdir -p "$t/decoy-d/sshd_config.d"
write_cfg "$t/decoy-d/sshd_config" no
sed -i "s|^Include .*|Include $t/decoy-d/sshd_config.d/*|" "$t/decoy-d/sshd_config"
echo "GatewayPorts no" > "$t/decoy-d/sshd_config.d/10-locked.conf"
chmod 000 "$t/decoy-d/sshd_config.d/10-locked.conf"
if [ "$(id -u)" != 0 ]; then
    start_sshd "$cfg" && refused "d. an unreadable Include" "$t/decoy-d/sshd_config" 'cannot read .*10-locked.conf|sshd -G could not'
fi
stop_sshd

# --- e. the post-start check, in a private network namespace ------------------
write_cfg "$t/run-yes.conf" yes
write_cfg "$t/decoy.conf" no
if unshare -rn true 2>/dev/null; then
    uid="$(id -u)"; gid="$(id -g)"
    # New ports: these are bound inside the namespace, not here.
    res="$(unshare -rn sh -c 'ip link set lo up 2>/dev/null || exit 9
        exec unshare --user --map-user="$1" --map-group="$2" "$3" --netns-case "$4" "$5" "$6" "$7"' \
        dmt "$uid" "$gid" "$0" "$t" "$sport" "$fport" "$hport" 2>&1 | tail -n 1)"
    case "$res" in
        NETNS-OK*) ok "e. sshd really binds the wildcard (${res#NETNS-OK }): the post-start lsof check killed it (78), the port closed" ;;
        *) bad "e. $res: $(cat "$t/netns.out" 2>/dev/null)" ;;
    esac
else
    ok "(e. skipped: no unprivileged user/network namespaces here)"
fi

printf '\n%d passed, %d failed\n' "$pass" "$failures"
[ "$failures" = 0 ]
