#!/usr/bin/env bash
# Copy the hub's network API bearer token to the Mac app (`just mac-token`).
#
#   scripts/mac-provision-token.sh [--host HOST] [--remote-dir DIR]
#
# Reads the token with `dictated --api-token` (which creates it if absent) and
# writes it over SSH to the Mac app's token file:
#
#   ~/Library/Application Support/DictateHub/api-token   (file 0600, dir 0700)
#
# The token never reaches stdout, a log, a command line (it travels on ssh's
# stdin, so no `ps` on either host can see it) or the repository. The script
# prints only where it wrote and whether the Mac's copy matches the hub's.
#
# Rotating: `dictated --rotate-api-token >/dev/null && just mac-token`. A
# running dictated accepts only the new token from its next request on.
#
# Environment:
#   DICTATE_MAC_HOST   default host when --host is not given (default mac-mini)
#   DICTATED           the dictated binary (default: `dictated` on PATH)
#   DICTATED_CONFIG    passed to dictated as --config (default: its own default)
set -euo pipefail

host="${DICTATE_MAC_HOST:-mac-mini}"
remote_dir='Library/Application Support/DictateHub'
dictated="${DICTATED:-dictated}"

usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; }
die() { printf 'mac-token: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --host) [ $# -ge 2 ] || die "--host needs a value"; host="$2"; shift 2 ;;
        --remote-dir) [ $# -ge 2 ] || die "--remote-dir needs a value"; remote_dir="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1 (see --help)" ;;
    esac
done

# The remote side receives the directory as a single-quoted argument (never the
# token), so it must not contain characters that could end that quoting.
case "$remote_dir" in
    *"'"*|*'"'*|*'\'*|*'$'*|*'`'*) die "--remote-dir must not contain quotes, backslashes, \$ or backticks" ;;
esac

command -v "$dictated" >/dev/null 2>&1 || die "'$dictated' not found; install it (just install) or set DICTATED"
command -v ssh >/dev/null 2>&1 || die "ssh not found"

args=(--api-token)
[ -n "${DICTATED_CONFIG:-}" ] && args+=(--config "$DICTATED_CONFIG")

# stdout carries only the token; dictated's notes on stderr are kept out of the
# terminal too, since they are not needed here.
token="$("$dictated" "${args[@]}" 2>/dev/null)" || die "dictated --api-token failed (run it yourself to see why)"
token="${token%%[[:space:]]*}"
case "$token" in
    dct1_*) ;;
    *) unset token; die "dictated --api-token did not print a token" ;;
esac

local_sum="$(printf '%s' "$token" | sha256sum | cut -d' ' -f1)"

# One remote shell: create the directory 0700, write a 0600 temp file from
# stdin under umask 077, rename it into place (atomic; no window where a
# partial or world-readable token exists), then print the file's SHA-256 and
# mode so the hub can confirm without reading the token back.
# shellcheck disable=SC2016  # expanded by the remote shell, on purpose
remote_script='
set -eu
umask 077
case "$1" in /*) dir="$1" ;; *) dir="$HOME/$1" ;; esac
mkdir -p "$dir"
chmod 700 "$dir"
tmp="$dir/.api-token.$$"
trap "rm -f \"$tmp\"" EXIT
cat > "$tmp"
chmod 600 "$tmp"
mv -f "$tmp" "$dir/api-token"
trap - EXIT
if command -v shasum >/dev/null 2>&1; then
    sum=$(shasum -a 256 < "$dir/api-token" | cut -d" " -f1)
else
    sum=$(sha256sum < "$dir/api-token" | cut -d" " -f1)
fi
if [ "$(uname)" = Darwin ]; then
    mode=$(stat -f %Lp "$dir/api-token"); dmode=$(stat -f %Lp "$dir")
else
    mode=$(stat -c %a "$dir/api-token"); dmode=$(stat -c %a "$dir")
fi
printf "%s %s %s %s\n" "$sum" "$mode" "$dmode" "$dir/api-token"
'

# printf is a shell builtin: the token is on no argv. ssh gets it on stdin.
reply="$(printf '%s' "$token" | ssh -o BatchMode=yes -o ConnectTimeout=15 "$host" \
    "sh -c '$remote_script' mac-token '$remote_dir'")" || { unset token; die "writing the token on $host failed"; }
unset token

read -r remote_sum mode dmode path <<<"$reply" || true
[ "$remote_sum" = "$local_sum" ] || die "the token on $host does not match the hub's (write failed?)"
[ "$mode" = "600" ] || die "$host:$path has mode $mode, expected 600"
[ "$dmode" = "700" ] || die "the token directory on $host has mode $dmode, expected 700"
printf 'mac-token: wrote the API token to %s:%s (0600, dir 0700); it matches the hub.\n' "$host" "$path"
