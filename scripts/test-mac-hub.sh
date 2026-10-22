#!/usr/bin/env bash
# Hermetic tunnel, token and documentation checks; no real Mac or credentials.
set -uo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
wrapper="$repo/scripts/dictate-mac-tunnel"
provision="$repo/scripts/mac-provision-token.sh"
t="$(mktemp -d /tmp/dmt-test.XXXXXX)"
trap 'rm -rf "$t"' EXIT
pass=0; failures=0
ok() { pass=$((pass + 1)); printf 'ok   %s\n' "$*"; }
bad() { failures=$((failures + 1)); printf 'FAIL %s\n' "$*" >&2; }
mkdir -p "$t/bin" "$t/stub" "$t/private run"
export DMT_STUB_DIR="$t/stub"
cat > "$t/bin/ssh" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$DMT_STUB_DIR/ssh.log"
[ -n "${DMT_STUB_FAIL:-}" ] && exit 255
for a in "$@"; do
    [ "$a" = -G ] && { printf '%s\n' "${DMT_CONFIG:-hostname stubmac}"; exit; }
    [ "$a" = -N ] && { touch "$DMT_STUB_DIR/tunnel.started"; exit "${DMT_TUNNEL_RC:-0}"; }
done
[ -n "${DMT_HANG:-}" ] && exec sleep 300
exec sh -c "${!#}"
SH
chmod +x "$t/bin/ssh"
reset_stub() { rm -f "$t/stub/"*; }
run() {
    reset_stub
    out=$(env DICTATE_MAC_SSH="$t/bin/ssh" DICTATE_MAC_HOST=stubmac \
        DICTATE_MAC_SOCKET="$t/private run/hub.sock" DICTATE_HUB_API_PORT=17998 \
        DICTATE_MAC_PREPARE_SECS=1 "$@" 2>&1)
    rc=$?
}
expect() {
    local name=$1 want=$2; shift 2
    run "$@"
    [ "$rc" = "$want" ] && ok "$name" || bad "$name: rc=$rc $out"
}
expect 'private directory check' 0 "$wrapper" --check
[ ! -f "$t/stub/tunnel.started" ] && ok '--check never starts a forward' || bad '--check forwarded'
[ "$(stat -c %a "$t/private run")" = 700 ] && ok 'socket directory secured 0700' || bad 'directory mode'
mkdir -p "$t/home/Library/Application Support"
expect 'default socket resolves Mac HOME' 0 HOME="$t/home" DICTATE_MAC_SOCKET= "$wrapper"
grep -qF -- "-R $t/home/Library/Application Support/DictateHub/run/hub.sock:127.0.0.1:17998" "$t/stub/ssh.log" \
    && ok 'default socket path matches app default' || bad 'default path mismatch'
expect 'Unix forward accepts spaces in socket path' 0 "$wrapper"
grep -qF -- "-R $t/private run/hub.sock:127.0.0.1:17998" "$t/stub/ssh.log" \
    && ok 'only explicit UDS reverse forward requested' || bad 'forward shape'
grep -qF -- 'ClearAllForwardings=yes' "$t/stub/ssh.log" && ok 'preparation is forward-free' || bad 'preparation options'
python3 - "$t/private run/hub.sock" <<'SOCKET'
import socket, sys
s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()
SOCKET
expect 'stale socket is removed before forwarding' 0 "$wrapper"
[ ! -e "$t/private run/hub.sock" ] && ok 'stale socket unlinked' || bad 'stale socket remains'
printf sentinel > "$t/private run/hub.sock"
expect 'regular file refused without clobbering' 78 "$wrapper"
[ "$(cat "$t/private run/hub.sock")" = sentinel ] && ok 'regular destination untouched' || bad 'file clobbered'
rm "$t/private run/hub.sock"
ln -s "$t/victim" "$t/private run/hub.sock"
expect 'destination symlink refused' 78 "$wrapper"
rm "$t/private run/hub.sock"
ln -s "$t/private run" "$t/link"
expect 'directory symlink refused' 78 DICTATE_MAC_SOCKET="$t/link/hub.sock" "$wrapper"
expect 'relative socket refused' 78 DICTATE_MAC_SOCKET=relative/hub.sock "$wrapper"
expect 'long Darwin path refused' 78 DICTATE_MAC_SOCKET="$t/private run/$(printf 's%.0s' {1..105})" "$wrapper"
expect 'colon refused' 78 DICTATE_MAC_SOCKET=/tmp/a:b "$wrapper"
expect 'bad port refused' 78 DICTATE_HUB_API_PORT=7313x "$wrapper"
expect 'option-like host refused' 78 DICTATE_MAC_HOST=-oProxyCommand=x "$wrapper"
expect 'configured TCP reverse forward refused' 78 DMT_CONFIG='remoteforward 0.0.0.0:99 127.0.0.1:88' "$wrapper"
[ ! -f "$t/stub/tunnel.started" ] && ok 'configured forward refused before SSH connection' || bad 'configured TCP forward started'
expect 'unreachable Mac retries' 1 DMT_STUB_FAIL=1 "$wrapper"
expect 'hung authenticated preparation has local deadline' 1 DMT_HANG=1 "$wrapper"
[ ! -f "$t/stub/tunnel.started" ] && ok 'hung preparation never starts a forward' || bad 'hung preparation forwarded'
expect 'running sshd refusal exits for retry' 255 DMT_TUNNEL_RC=255 "$wrapper"
# Source must never request a remote TCP listener or invoke a post-start checker.
if grep -qE -- '-R ["\x27]?127\.0\.0\.1:|remote_listeners|remote_policy|lsof' "$wrapper"; then
    bad 'legacy TCP/checker transport remains'
else ok 'no TCP -R or post-start checker in wrapper'; fi

# --- token provisioning (#3557) ----------------------------------------------
token="dct1_$(printf 'Z%.0s' $(seq 1 43))"
cat > "$t/bin/dictated" <<SH
#!/bin/sh
[ "\$1" = --api-token ] || exit 2
echo "a note on stderr" >&2
echo "$token"
SH
chmod +x "$t/bin/dictated"
provision() { # remote dir; sets rc, out
    reset_stub
    out="$(DICTATED="$t/bin/dictated" DICTATE_MAC_SSH="$t/bin/ssh" "$provision" --host stubmac --remote-dir "$1" 2>&1)"
    rc=$?
}
mode_of() { stat -c %a "$1"; }
leftovers() { find "$1" -maxdepth 1 -name '.api-token.*' ! -type l | wc -l; }
no_token_leak() { # name
    if grep -qF "$token" <<<"$out" || grep -qF "$token" "$t/stub/ssh.log"; then
        bad "$1: the token reached stdout/stderr or ssh's argv"
    fi
}

d="$t/mac/fresh/DictateHub"
provision "$d"
[ "$rc" = 0 ] && [ "$(cat "$d/api-token")" = "$token" ] && [ "$(mode_of "$d/api-token")" = 600 ] \
    && [ "$(mode_of "$d")" = 700 ] && [ "$(leftovers "$d")" = 0 ] \
    && ok "token: fresh dir -> 0600 file, 0700 dir, exact content, no temp left" \
    || bad "token fresh: rc=$rc $out"
no_token_leak "token fresh"

# Pre-planted symlinks: the old predictable temp names (.api-token.<pid>, for
# the next 2000 pids, which the remote sh will almost surely be one of) and
# api-token itself, all pointing at a victim file outside the directory; the
# dir starts 0755. (Against the pre-#3557 script this clobbers the victim.)
d="$t/mac/planted/DictateHub"; victim="$t/outside/victim"
mkdir -p "$d" "$t/outside"; chmod 755 "$d"
echo VICTIM > "$victim"
# One process plants them all (a loop of `ln` would itself use up those pids).
planted="$(python3 -I -c '
import os, sys
victim, d = sys.argv[1], sys.argv[2]
me = os.getpid()
for pid in range(me + 1, me + 2001):
    os.symlink(victim, os.path.join(d, ".api-token.%d" % pid))
os.symlink(victim, os.path.join(d, "api-token"))
print(2000)
' "$victim" "$d")"
provision "$d"
[ "$rc" = 0 ] && [ "$(cat "$victim")" = VICTIM ] && [ ! -L "$d/api-token" ] && [ -f "$d/api-token" ] \
    && [ "$(cat "$d/api-token")" = "$token" ] && [ "$(mode_of "$d")" = 700 ] \
    && ok "token: pre-planted symlinks are neither followed nor clobbered" \
    || bad "token planted: rc=$rc victim=$(cat "$victim") $out"
[ "$(find "$d" -maxdepth 1 -name '.api-token.*' -type l | wc -l)" = "$planted" ] && [ "$(leftovers "$d")" = 0 ] \
    && ok "  ... the planted temp-name links are untouched, no temp file left" || bad "planted temp links changed"

d="$t/mac/dirlink/DictateHub"; mkdir -p "$d" "$t/outside/dir"
ln -s "$t/outside/dir" "$d/api-token"
provision "$d"
[ "$rc" = 0 ] && [ -z "$(ls -A "$t/outside/dir")" ] && [ -f "$d/api-token" ] && [ ! -L "$d/api-token" ] \
    && ok "token: api-token -> a directory symlink is replaced, nothing written into the directory" \
    || bad "token dirlink: rc=$rc $(ls -A "$t/outside/dir") $out"

d="$t/mac/realdir/DictateHub"; mkdir -p "$d/api-token"
provision "$d"
[ "$rc" != 0 ] && [ -z "$(ls -A "$d/api-token")" ] && [ "$(leftovers "$d")" = 0 ] \
    && ok "token: api-token as a real directory is refused, no temp left" || bad "token realdir: rc=$rc $out"

mkdir -p "$t/outside/target"; ln -s "$t/outside/target" "$t/mac/linkdir"
provision "$t/mac/linkdir"
[ "$rc" != 0 ] && [ -z "$(ls -A "$t/outside/target")" ] && grep -q "symlink" <<<"$out" \
    && ok "token: a symlinked token directory is refused" || bad "token linkdir: rc=$rc $out"
no_token_leak "token linkdir"

# --- docs: the documented check keeps the token off argv (#3556) -------------
# Pull the remote half of every documented `ssh mac-mini '... curl ...'` line
# and run it with a fake curl that records argv and stdin.
mkdir -p "$t/home/Library/Application Support/DictateHub" "$t/curlbin"
printf '%s' "$token" > "$t/home/Library/Application Support/DictateHub/api-token"
cat > "$t/curlbin/curl" <<'SH'
#!/bin/sh
printf '%s\n' "$*" >> "$DMT_STUB_DIR/curl.argv"
cat >> "$DMT_STUB_DIR/curl.stdin" 2>/dev/null || true
SH
chmod +x "$t/curlbin/curl"
n=0
while IFS= read -r line; do
    inner="${line#ssh mac-mini \'}"
    inner="${inner%\'}"
    reset_stub
    HOME="$t/home" PATH="$t/curlbin:$PATH" sh -c "$inner" < /dev/null
    n=$((n + 1))
    if grep -qF "$token" "$t/stub/curl.argv" 2>/dev/null; then
        bad "docs: the token is on curl's argv in: $line"
    fi
    if grep -q -- '-K -' <<<"$line" && ! grep -qF "$token" "$t/stub/curl.stdin" 2>/dev/null; then
        bad "docs: the -K - form did not deliver the header on stdin: $line"
    fi
done < <(grep -E "^ *ssh mac-mini '.*curl" "$repo/docs/MAC-APP.md" | sed 's/^ *//')
[ "$n" -ge 2 ] && ok "docs: $n documented curl checks keep the token off argv" || bad "docs: found $n curl lines"
if grep -rnE -- "-H +[\"']?Authorization: Bearer +[\$\`]" "$repo/docs" "$repo/scripts" "$repo/README.md" \
    "$repo/CLAUDE.md" "$repo/AGENTS.md" 2>/dev/null; then
    bad "a curl -H puts an expanded bearer token on argv (above)"
else
    ok "no doc or script expands a bearer token into curl -H"
fi

printf '\n%d passed, %d failed\n' "$pass" "$failures"
[ "$failures" = 0 ]
