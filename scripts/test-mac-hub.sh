#!/usr/bin/env bash
# Hermetic regression tests for the Mac app's hub side (part of `just
# check-cpu`; `just test-mac-hub` alone). No network, no real Mac, no real
# sshd, no real token:
#
#   * scripts/dictate-mac-tunnel  (#3555) the sshd-policy preflight refuses
#     every unsafe or unverifiable GatewayPorts, Match blocks and nested
#     Includes included, and the post-start listener check kills a tunnel the
#     Mac publishes beyond loopback. A stub `ssh` runs the "remote" half
#     locally, against fixture configs, a fake `sshd -G` and a fake `lsof`.
#   * scripts/mac-provision-token.sh  (#3557) never follows or clobbers a
#     pre-planted symlink, and leaves no temp file behind.
#   * docs/MAC-APP.md  (#3556) the documented check command keeps the token
#     off every argv (run with a fake curl that records its argv).
#
# The real-sshd counterpart (an isolated sshd on 127.0.0.1, plus a private
# network namespace for the wildcard case) is scripts/mac-tunnel-sshd-test.sh.
set -uo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
wrapper="$repo/scripts/dictate-mac-tunnel"
provision="$repo/scripts/mac-provision-token.sh"

t="$(mktemp -d /tmp/dmt-test.XXXXXX)"
wrapper_pid=""
cleanup() {
    [ -n "$wrapper_pid" ] && kill "$wrapper_pid" 2>/dev/null
    chmod -R u+rwx "$t" 2>/dev/null
    rm -rf "$t"
}
trap cleanup EXIT

pass=0
failures=0
ok() { pass=$((pass + 1)); printf 'ok   %s\n' "$*"; }
bad() { failures=$((failures + 1)); printf 'FAIL %s\n' "$*" >&2; }

mkdir -p "$t/bin" "$t/stub"
export DMT_STUB_DIR="$t/stub"

# --- stubs -------------------------------------------------------------------
# ssh: with -N it is the tunnel (records itself, sleeps until killed);
# otherwise it runs its last argument locally, as the remote login shell would.
cat > "$t/bin/ssh" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$DMT_STUB_DIR/ssh.log"
[ -n "${DMT_STUB_FAIL:-}" ] && exit 255
for a in "$@"; do
    if [ "$a" = -N ]; then
        [ -n "${DMT_STUB_TUNNEL_EXIT:-}" ] && exit "$DMT_STUB_TUNNEL_EXIT"
        echo $$ > "$DMT_STUB_DIR/tunnel.pid"
        trap 'touch "$DMT_STUB_DIR/tunnel.killed"; exit 0' TERM
        sleep 300 & wait
        exit 0
    fi
done
exec sh -c "${!#}"
SH
# sshd -G: reports DMT_FAKE_G (default no); "fail" = sshd -G errors out.
cat > "$t/bin/sshd" <<'SH'
#!/bin/sh
[ "${DMT_FAKE_G:-no}" = fail ] && { echo "sshd: config error: bad thing" >&2; exit 255; }
printf 'port 22\ngatewayports %s\nx11forwarding no\n' "${DMT_FAKE_G:-no}"
SH
# lsof: once the tunnel stub runs, lists DMT_FAKE_LISTEN's addresses.
cat > "$t/bin/lsof" <<'SH'
#!/bin/sh
[ -f "$DMT_STUB_DIR/tunnel.pid" ] || exit 1
[ -n "${DMT_FAKE_LISTEN:-}" ] || exit 1
for a in $DMT_FAKE_LISTEN; do printf 'p4242\nf7\nn%s\n' "$a"; done
SH
chmod +x "$t/bin/ssh" "$t/bin/sshd" "$t/bin/lsof"

etc="$t/etc/ssh"
reset_stub() { rm -f "$t/stub/"*; }
# Write the macOS-shaped fixture: sshd_config Includes sshd_config.d/* (as a
# relative path here, to cover that too), 100-macos.conf Includes a file
# outside that directory (as macOS does with crypto.conf).
fixture() {
    chmod -R u+rwx "$t/etc" 2>/dev/null
    rm -rf "$t/etc"
    mkdir -p "$etc/sshd_config.d" "$etc/crypto"
    cat > "$etc/sshd_config" <<'CFG'
# a comment
#GatewayPorts yes
Include sshd_config.d/*
AuthorizedKeysFile	.ssh/authorized_keys
CFG
    printf 'UsePAM yes\nInclude %s/crypto.conf\n' "$etc" > "$etc/sshd_config.d/100-macos.conf"
    printf 'Ciphers ^aes128-gcm@openssh.com\n' > "$etc/crypto/apple.conf"
    ln -s crypto/apple.conf "$etc/crypto.conf"
}

run() { # env... -- runs the wrapper; sets rc and out
    out="$(env DICTATE_MAC_SSH="$t/bin/ssh" DICTATE_MAC_SSHD="$t/bin/sshd" \
        DICTATE_MAC_LSOF="$t/bin/lsof" DICTATE_MAC_SSHD_CONFIG="$etc/sshd_config" \
        DICTATE_MAC_HOST=stubmac DICTATE_MAC_PORT=17999 DICTATE_HUB_API_PORT=17998 \
        DICTATE_MAC_VERIFY_SECS=2 "$@" 2>&1)"
    rc=$?
}
expect_check() { # name, expected rc, grep pattern for output ("" = none), env...
    local name="$1" want="$2" pat="$3"
    shift 3
    reset_stub
    run "$@" "$wrapper" --check
    if [ "$rc" != "$want" ]; then bad "$name: exit $rc, expected $want: $out"; return; fi
    if [ -n "$pat" ] && ! grep -qE -- "$pat" <<<"$out"; then bad "$name: output lacks /$pat/: $out"; return; fi
    if [ -f "$t/stub/tunnel.pid" ]; then bad "$name: a tunnel was started by --check"; return; fi
    ok "$name"
}

# --- preflight: safe policies pass -------------------------------------------
fixture
expect_check "macOS-shaped default config passes" 0 "preflight ok.*3 config path"
grep -q -- '-o ClearAllForwardings=yes' "$t/stub/ssh.log" && grep -q -- '-o BatchMode=yes' "$t/stub/ssh.log" \
    && ok "check connections run with BatchMode and no forwards" || bad "check connection options: $(cat "$t/stub/ssh.log")"

fixture; printf 'GatewayPorts no # never yes here\n' >> "$etc/sshd_config"
expect_check "explicit 'GatewayPorts no' with a trailing comment passes" 0 "preflight ok"

# --- preflight: every unsafe or unverifiable policy is refused (78) ----------
fixture; printf 'GatewayPorts yes\n' >> "$etc/sshd_config"
expect_check "GatewayPorts yes in sshd_config" 78 'GatewayPorts "yes" in .*/sshd_config'
fixture; printf 'gatewayports=Yes\n' >> "$etc/sshd_config"
expect_check "gatewayports=Yes (case, '=' separator)" 78 'GatewayPorts "Yes"'
fixture; printf 'GatewayPorts "yes"\n' >> "$etc/sshd_config"
expect_check "quoted \"yes\"" 78 'GatewayPorts'
fixture; printf '  GatewayPorts clientspecified\n' >> "$etc/sshd_config.d/100-macos.conf"
expect_check "clientspecified in an included file" 78 'clientspecified.*100-macos.conf'
fixture; printf 'Match User someone\n    GatewayPorts yes\n' >> "$etc/sshd_config.d/100-macos.conf"
expect_check "GatewayPorts yes inside a Match block" 78 'inside "Match User someone"'
fixture; printf 'Include %s/deeper.conf\n' "$etc" >> "$etc/crypto/apple.conf"
printf 'Match Address 10.0.0.0/8\n\tGatewayPorts yes\n' > "$etc/deeper.conf"
expect_check "yes three Includes deep, inside Match" 78 'deeper.conf \(inside "Match Address'
fixture; printf 'GatewayPorts\n' >> "$etc/sshd_config"
expect_check "GatewayPorts with no value" 78 'GatewayPorts ""'
fixture; printf 'GatewayPorts no yes\n' >> "$etc/sshd_config"
expect_check "GatewayPorts with two values" 78 'GatewayPorts "no yes"'
fixture; printf 'Include ~/x.conf\n' >> "$etc/sshd_config"
expect_check "an Include it cannot follow (~)" 78 'cannot follow Include'
fixture; printf 'Include "%s/unterminated\n' "$etc" >> "$etc/sshd_config"
expect_check "an unterminated quote" 78 'unbalanced quote'
if [ "$(id -u)" != 0 ]; then
    fixture; chmod 000 "$etc/crypto/apple.conf"
    expect_check "an unreadable included file" 78 'cannot read .*crypto.conf'
    fixture; chmod 000 "$etc/sshd_config"
    expect_check "an unreadable sshd_config" 78 'cannot read'
else
    ok "(skipped unreadable-file cases: running as root)"
fi
fixture; rm "$etc/sshd_config"
expect_check "a missing sshd_config" 78 'cannot read .*sshd_config'
fixture
expect_check "sshd -G reports yes (files say no)" 78 'effective GatewayPorts is "yes"' DMT_FAKE_G=yes
expect_check "sshd -G fails" 78 'sshd -G could not report' DMT_FAKE_G=fail
expect_check "no sshd binary on the Mac" 78 'no sshd binary' DICTATE_MAC_SSHD=/nonexistent/sshd
expect_check "the Mac is unreachable: retry, not refuse" 1 'ssh failed' DMT_STUB_FAIL=1
expect_check "a non-numeric port" 78 'is not a port' DICTATE_MAC_PORT=7313x
expect_check "a host that looks like an option" 78 'is not a host' DICTATE_MAC_HOST=-oProxyCommand=x

# --- the parser alone: truncation and garbage --------------------------------
v="$(printf '@@G gatewayports no\n=== /e/sshd_config\n| Port 22\n' | "$wrapper" --parse-sshd-dump /e/sshd_config)"
grep -q '^REFUSE .*truncated' <<<"$v" && ok "a dump without @@END is refused" || bad "truncated dump: $v"
v="$(printf '@@G gatewayports no\nsurprise\n@@END\n' | "$wrapper" --parse-sshd-dump /e/sshd_config)"
grep -q '^REFUSE unexpected line' <<<"$v" && ok "an unexpected line is refused" || bad "garbage dump: $v"
v="$(printf '@@G gatewayports no\n=== /e/sshd_config\n| Include a/*.conf "/b c/d"\n@@END\n' | "$wrapper" --parse-sshd-dump /e/sshd_config)"
grep -qx 'INCLUDE /e/a/\*.conf' <<<"$v" && grep -qx 'INCLUDE /b c/d' <<<"$v" && ! grep -q REFUSE <<<"$v" \
    && ok "Include patterns: relative resolved, quotes grouped" || bad "include parsing: $v"

# --- the full run: post-start listener verification --------------------------
start_wrapper() { # env...
    reset_stub
    env DICTATE_MAC_SSH="$t/bin/ssh" DICTATE_MAC_SSHD="$t/bin/sshd" \
        DICTATE_MAC_LSOF="$t/bin/lsof" DICTATE_MAC_SSHD_CONFIG="$etc/sshd_config" \
        DICTATE_MAC_HOST=stubmac DICTATE_MAC_PORT=17999 DICTATE_HUB_API_PORT=17998 \
        DICTATE_MAC_VERIFY_SECS=2 "$@" "$wrapper" > "$t/wrapper.log" 2>&1 &
    wrapper_pid=$!
}
finish_wrapper() { # waits up to 10 s; sets rc
    local i
    for i in $(seq 1 100); do kill -0 "$wrapper_pid" 2>/dev/null || break; sleep 0.1; done
    if kill -0 "$wrapper_pid" 2>/dev/null; then rc=running; else wait "$wrapper_pid"; rc=$?; fi
    wrapper_pid=""
}
tunnel_alive() { [ -f "$t/stub/tunnel.pid" ] && kill -0 "$(cat "$t/stub/tunnel.pid")" 2>/dev/null; }

fixture
for listen in "127.0.0.1:17999" "[::1]:17999" "127.0.0.1:17999 [::1]:17999"; do
    start_wrapper DMT_FAKE_LISTEN="$listen"
    for _ in $(seq 1 50); do grep -q "tunnel up" "$t/wrapper.log" && break; sleep 0.1; done
    if grep -q "tunnel up" "$t/wrapper.log" && tunnel_alive && kill -0 "$wrapper_pid" 2>/dev/null; then
        kill -TERM "$wrapper_pid"
        finish_wrapper
        [ "$rc" = 0 ] && [ -f "$t/stub/tunnel.killed" ] \
            && ok "loopback listener ($listen): tunnel stays up; SIGTERM stops it (exit 0)" \
            || bad "stop after '$listen': rc=$rc killed=$([ -f "$t/stub/tunnel.killed" ] && echo y)"
    else
        bad "loopback listener ($listen) did not come up: $(cat "$t/wrapper.log")"
        kill "$wrapper_pid" 2>/dev/null; finish_wrapper
    fi
done
grep -qx -- '.*-N -T -o ExitOnForwardFailure=yes .* -R 127.0.0.1:17999:127.0.0.1:17998 stubmac' "$t/stub/ssh.log" \
    && ok "the tunnel requests -R 127.0.0.1:MAC_PORT:127.0.0.1:HUB_PORT" || bad "tunnel argv: $(grep -- -N "$t/stub/ssh.log")"
grep -- ' -N ' "$t/stub/ssh.log" | grep -q ClearAllForwardings && bad "the tunnel must not clear its own forward" \
    || ok "the tunnel does not clear its own -R"

expect_run() { # name, expected rc, pattern, env...
    local name="$1" want="$2" pat="$3"
    shift 3
    start_wrapper "$@"
    finish_wrapper
    local log
    log="$(cat "$t/wrapper.log")"
    if [ "$rc" != "$want" ]; then bad "$name: exit $rc, expected $want: $log"; return; fi
    if ! grep -qE -- "$pat" <<<"$log"; then bad "$name: log lacks /$pat/: $log"; return; fi
    if tunnel_alive; then bad "$name: the tunnel is still running"; return; fi
    ok "$name"
}
expect_run "wildcard listener on the Mac: tunnel killed, refused" 78 'beyond its loopback \(\*:17999\); tunnel killed' \
    DMT_FAKE_LISTEN='*:17999'
[ -f "$t/stub/tunnel.killed" ] && ok "  ... and the tunnel process got SIGTERM" || bad "wildcard: tunnel not killed"
expect_run "loopback plus a LAN address: refused" 78 '192.168.1.20:17999' \
    DMT_FAKE_LISTEN='127.0.0.1:17999 192.168.1.20:17999'
expect_run "IPv6 wildcard: refused" 78 '\[::\]:17999' DMT_FAKE_LISTEN='[::]:17999'
expect_run "no listener appears: killed, retry (1)" 1 'no listener' DMT_FAKE_LISTEN=
expect_run "no lsof on the Mac: killed, refused" 78 'no lsof' DMT_FAKE_LISTEN=127.0.0.1:17999 \
    DICTATE_MAC_LSOF=/nonexistent/lsof
expect_run "the tunnel's ssh dies at once: retry (1)" 1 'exited during startup \(status 255\)' \
    DMT_FAKE_LISTEN= DMT_STUB_TUNNEL_EXIT=255
printf 'GatewayPorts yes\n' >> "$etc/sshd_config"
expect_run "unsafe policy, full run: refused before any forward" 78 'REFUSED' DMT_FAKE_LISTEN=127.0.0.1:17999
[ ! -f "$t/stub/tunnel.pid" ] && ! grep -q -- ' -N ' "$t/stub/ssh.log" \
    && ok "  ... and ssh -R was never run" || bad "unsafe policy still started a tunnel"

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
