#!/bin/sh
# Plain-sh entry point; Python supplies synthetic processes and assertions.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cutover_test_root=$(mktemp -d /tmp/cutover-harness-XXXXXX)
trap 'rm -rf -- "$cutover_test_root"' EXIT HUP INT TERM
mkdir -p "$cutover_test_root/home/.config" "$cutover_test_root/home/.local/share" \
    "$cutover_test_root/home/.cache" "$cutover_test_root/home/run" "$cutover_test_root/bin"
printf '#!/bin/sh\necho "harness: real systemctl is forbidden" >&2\nexit 99\n' > "$cutover_test_root/bin/systemctl"
chmod +x "$cutover_test_root/bin/systemctl"
export HOME="$cutover_test_root/home"
export XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share" \
    XDG_CACHE_HOME="$HOME/.cache" XDG_RUNTIME_DIR="$HOME/run"
export PATH="$cutover_test_root/bin:$PATH"
unset DICTATE_SOCKET DBUS_SESSION_BUS_ADDRESS
python3 "$script_dir/cutover-test.py"
