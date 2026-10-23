#!/bin/bash
# Linux or Mac: assert service lifecycle and paths without touching launchd.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
export HOME="$root/home & test"
mkdir -p "$HOME/Library" "$root/bin"
export PATH="$root/bin:$PATH"
export CALLS="$root/calls"
export LOADED="$root/loaded"
cat > "$root/bin/launchctl" <<'STUB'
#!/bin/bash
printf '%s\n' "$*" >> "$CALLS"
case $1 in
    print) [[ -f $LOADED ]] ;;
    bootstrap) touch "$LOADED" ;;
    bootout) rm -f "$LOADED" ;;
    kickstart) [[ -f $LOADED ]] ;;
esac
STUB
cat > "$root/bin/dictated" <<'STUB'
#!/bin/bash
printf 'dictated %s\n' "$*" >> "$CALLS"
case $* in *--api-token*) echo 'SYNTHETIC_TOKEN_NEVER_IN_CALLS';; esac
STUB
cat > "$root/bin/dictate" <<'STUB'
#!/bin/bash
printf 'dictate %s\n' "$*" >> "$CALLS"
STUB
chmod +x "$root/bin/"*
export DICTATED_BINARY="$root/bin/dictated" DICTATE_CLI="$root/bin/dictate"
"$HERE/dictated-launchagent.sh" install > "$root/output"
data="$HOME/Library/Application Support/dictated"
plist="$HOME/Library/LaunchAgents/dev.jakedevar.dictated.plist"
[[ -f $plist && -x $data/bin/dictated ]]
grep -q '<key>KeepAlive</key><true/>' "$plist"
grep -q '<key>RunAtLoad</key><true/>' "$plist"
grep -q 'home &amp; test/Library/Application Support/dictated/bin/dictated' "$plist"
grep -q 'bootstrap gui/' "$CALLS"
grep -q 'kickstart -k gui/' "$CALLS"
grep -q 'model pull large-v3-turbo' "$CALLS"
! grep -q SYNTHETIC_TOKEN "$root/output"
[[ $(cat "$data/config.toml") == *'capture = false'* ]]
printf '\n# preserve me\n' >> "$data/config.toml"
"$HERE/dictated-launchagent.sh" install >/dev/null
grep -q '# preserve me' "$data/config.toml"
[[ $(grep -c '^bootstrap ' "$CALLS") == 1 ]]
! grep -q '^bootout ' "$CALLS"
"$HERE/dictated-launchagent.sh" restart
"$HERE/dictated-launchagent.sh" status
"$HERE/dictated-launchagent.sh" uninstall > /dev/null
[[ ! -e $plist && -f $data/config.toml ]]
ln -s "$root/victim" "$plist"
if "$HERE/dictated-launchagent.sh" install >/dev/null 2>&1; then echo 'accepted symlink plist' >&2; exit 1; fi
[[ ! -e $root/victim ]]
echo 'LaunchAgent tests: 1 passed, 0 failed'
