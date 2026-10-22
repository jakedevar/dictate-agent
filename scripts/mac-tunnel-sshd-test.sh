#!/usr/bin/env bash
# Real, non-root isolated sshd: GatewayPorts yes must create only a private
# Unix socket. Temporary keys/config/loopback ports only; no real token/Mac.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d /tmp/dmt-sshd.XXXXXX)
http_pid= sshd_pid= tunnel_pid=
cleanup() {
    if [[ $? != 0 ]]; then tail -25 "$t/sshd.log" "$t/tunnel.log" >&2; fi
    for p in "$tunnel_pid" "$sshd_pid" "$http_pid"; do
        if [[ -n $p ]]; then kill "$p" 2>/dev/null || true; wait "$p" 2>/dev/null || true; fi
    done
    rm -rf "$t"
}
trap cleanup EXIT
pass=0
ok() { pass=$((pass+1)); printf 'ok   %s\n' "$*"; }
sshd_bin=$(command -v sshd || true)
[[ -n $sshd_bin ]] || sshd_bin=/usr/sbin/sshd
free_port() { python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }
sport=$(free_port); hport=$(free_port)
ssh-keygen -q -t ed25519 -N '' -f "$t/hostkey" -C test-host
ssh-keygen -q -t ed25519 -N '' -f "$t/id" -C test-client
cp "$t/id.pub" "$t/authorized_keys"
printf '[127.0.0.1]:%s %s\n' "$sport" "$(cut -d' ' -f1,2 "$t/hostkey.pub")" > "$t/known_hosts"
cat > "$t/ssh_config" <<CFG
Host test-mac
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
cat > "$t/sshd_config" <<CFG
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
AllowStreamLocalForwarding yes
StreamLocalBindUnlink no
StreamLocalBindMask 0177
GatewayPorts yes
CFG
"$sshd_bin" -t -f "$t/sshd_config"
"$sshd_bin" -D -e -f "$t/sshd_config" > "$t/sshd.log" 2>&1 &
sshd_pid=$!
mkdir "$t/www" "$t/private run"
printf 'hub-api-stand-in\n' > "$t/www/index.html"
python3 -I -m http.server --bind 127.0.0.1 --directory "$t/www" "$hport" > "$t/http.log" 2>&1 &
http_pid=$!
for _ in {1..50}; do "$t/ssh" -o BatchMode=yes -o StrictHostKeyChecking=yes test-mac true 2>/dev/null && break; sleep 0.1; done
"$t/ssh" -o BatchMode=yes -o StrictHostKeyChecking=yes test-mac true
ok 'isolated running sshd authenticates (GatewayPorts yes, TCP forwarding enabled)'
export DICTATE_MAC_SSH="$t/ssh" DICTATE_MAC_HOST=test-mac
export DICTATE_MAC_SOCKET="$t/private run/hub.sock" DICTATE_HUB_API_PORT="$hport"
start_tunnel() {
    "$repo/scripts/dictate-mac-tunnel" > "$t/tunnel.log" 2>&1 &
    tunnel_pid=$!
    for _ in {1..100}; do
        curl -fsS --max-time 1 --unix-socket "$DICTATE_MAC_SOCKET" "http://127.0.0.1:$hport/" >/dev/null 2>&1 && break
        kill -0 "$tunnel_pid" 2>/dev/null || { cat "$t/tunnel.log"; exit 1; }
        sleep 0.1
    done
    [[ -S $DICTATE_MAC_SOCKET ]]
}
start_tunnel
[[ $(stat -c %a "$t/private run") = 700 && $(stat -c %a "$DICTATE_MAC_SOCKET") = 600 ]]
ok 'only UDS created, private directory 0700 and socket 0600'
body=$(curl --fail --silent --show-error --max-time 5 --unix-socket "$DICTATE_MAC_SOCKET" "http://127.0.0.1:$hport/")
[[ $body = hub-api-stand-in ]]
ok 'HTTP bytes traverse UDS -> SSH -> hub TCP with GatewayPorts yes'
# The isolated sshd and its descendants have only the SSH listener itself.
listeners=$(lsof -nP -a -c sshd -iTCP -sTCP:LISTEN -Fn 2>/dev/null | sed -n 's/^n//p' | sort -u)
[[ $listeners = "127.0.0.1:$sport" ]]
ok 'GatewayPorts yes produced no forwarded TCP listener'
kill "$tunnel_pid"; wait "$tunnel_pid" || true; tunnel_pid=
if curl -s --max-time 2 --unix-socket "$DICTATE_MAC_SOCKET" "http://127.0.0.1:$hport/" > /dev/null; then exit 1; fi
ok 'stopped forward cannot carry traffic'
# OpenSSH leaves its inode (BindUnlink=no); wrapper must remove it to restart.
start_tunnel
[[ $(curl -fsS --max-time 5 --unix-socket "$DICTATE_MAC_SOCKET" "http://127.0.0.1:$hport/") = hub-api-stand-in ]]
ok 'restart removes stale socket and traffic flows again'
kill "$tunnel_pid"; wait "$tunnel_pid" || true; tunnel_pid=
kill "$sshd_pid"; wait "$sshd_pid" || true; sshd_pid=
sed -i 's/AllowStreamLocalForwarding yes/AllowStreamLocalForwarding no/' "$t/sshd_config"
"$sshd_bin" -D -e -f "$t/sshd_config" > "$t/sshd.log" 2>&1 &
sshd_pid=$!
for _ in {1..50}; do "$t/ssh" -o BatchMode=yes test-mac true 2>/dev/null && break; sleep 0.1; done
set +e
timeout 10 "$repo/scripts/dictate-mac-tunnel" > "$t/tunnel.log" 2>&1
rc=$?
set -e
[[ $rc = 1 ]]
ok 'running sshd denies stream-local forwarding: ExitOnForwardFailure exits'
cat >> "$t/ssh_config" <<CFG
    ClearAllForwardings yes
    RemoteForward 127.0.0.1:17999 127.0.0.1:17998
CFG
set +e
"$repo/scripts/dictate-mac-tunnel" --check > "$t/tunnel.log" 2>&1
rc=$?
set -e
[[ $rc = 78 ]]
ok 'configured TCP forward refused even when alias sets ClearAllForwardings yes'
printf '\n%d passed, 0 failed\n' "$pass"
