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
#   DICTATE_MAC_SSH    the ssh client (default ssh; scripts/test-mac-hub.sh
#                      points it at a stub that runs the remote side locally)
set -euo pipefail

host="${DICTATE_MAC_HOST:-mac-mini}"
remote_dir='Library/Application Support/DictateHub'
dictated="${DICTATED:-dictated}"
ssh_bin="${DICTATE_MAC_SSH:-ssh}"

usage() { sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'; }
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
command -v "$ssh_bin" >/dev/null 2>&1 || die "$ssh_bin not found"

args=(--api-token)
[ -n "${DICTATED_CONFIG:-}" ] && args+=(--config "$DICTATED_CONFIG")

# stdout carries only the token; dictated's notes on stderr are kept out of the
# terminal too, since they are not needed here.
token="$("$dictated" "${args[@]}" 2>/dev/null)" || die "dictated --api-token failed (run it yourself to see why)"
# Only the dct1_ line: never mistake anything else on stdout for the token.
token="$(printf '%s\n' "$token" | grep -m1 -E '^dct1_[A-Za-z0-9_-]{43}$' || true)"
case "$token" in
    dct1_*) ;;
    *) unset token; die "dictated --api-token did not print a token" ;;
esac

local_sum="$(printf '%s' "$token" | sha256sum | cut -d' ' -f1)"

# One remote shell, POSIX sh, umask 077 throughout:
#   * the directory: created 0700 if absent; refused if it is a symlink or not
#     ours (chmod would otherwise follow a planted link to someone else's dir);
#   * the temp file: `mktemp` in that directory, so its name is unpredictable
#     and it is created exclusively (O_EXCL: a pre-planted symlink or file is
#     never opened, followed or truncated); a trap removes it on any exit;
#   * a pre-existing api-token that is a symlink is unlinked (never followed);
#     a real directory there is refused (mv would move the file INTO it);
#   * rename(2) into place: atomic, so no partial or readable token exists.
# It prints the file's SHA-256 and modes so the hub can confirm without
# reading the token back.
# shellcheck disable=SC2016  # expanded by the remote shell, on purpose
remote_script='
set -eu
umask 077
case "$1" in /*) dir="$1" ;; *) dir="$HOME/$1" ;; esac
fail() { echo "mac-token(remote): $*" >&2; exit 1; }
[ -L "$dir" ] && fail "$dir is a symlink; refusing to write through it"
mkdir -p "$dir"
[ -d "$dir" ] || fail "$dir is not a directory"
[ -O "$dir" ] || fail "$dir is not owned by $(id -un)"
chmod 700 "$dir"
tmp=$(mktemp "$dir/.api-token.XXXXXXXX") || fail "mktemp in $dir failed"
trap "rm -f \"\$tmp\"" EXIT
trap "exit 1" HUP INT TERM
[ -f "$tmp" ] && [ ! -L "$tmp" ] && [ -O "$tmp" ] || fail "mktemp gave an unexpected file"
chmod 600 "$tmp"
cat > "$tmp"
dest="$dir/api-token"
if [ -L "$dest" ]; then rm -f "$dest"; fi
[ -d "$dest" ] && fail "$dest is a directory; remove it first"
mv -f "$tmp" "$dest"
trap - EXIT
[ -f "$dest" ] && [ ! -L "$dest" ] || fail "$dest is not a regular file after the rename"
if command -v shasum >/dev/null 2>&1; then
    sum=$(shasum -a 256 < "$dest" | cut -d" " -f1)
else
    sum=$(sha256sum < "$dest" | cut -d" " -f1)
fi
if [ "$(uname)" = Darwin ]; then
    mode=$(stat -f %Lp "$dest"); dmode=$(stat -f %Lp "$dir")
else
    mode=$(stat -c %a "$dest"); dmode=$(stat -c %a "$dir")
fi
printf "%s %s %s %s\n" "$sum" "$mode" "$dmode" "$dest"
'

# printf is a shell builtin: the token is on no argv. ssh gets it on stdin.
reply="$(printf '%s' "$token" | "$ssh_bin" -o BatchMode=yes -o ConnectTimeout=15 "$host" \
    "sh -c '$remote_script' mac-token '$remote_dir'")" || { unset token; die "writing the token on $host failed"; }
unset token

read -r remote_sum mode dmode path <<<"$reply" || true
[ "$remote_sum" = "$local_sum" ] || die "the token on $host does not match the hub's (write failed?)"
[ "$mode" = "600" ] || die "$host:$path has mode $mode, expected 600"
[ "$dmode" = "700" ] || die "the token directory on $host has mode $dmode, expected 700"
printf 'mac-token: wrote the API token to %s:%s (0600, dir 0700); it matches the hub.\n' "$host" "$path"
