#!/usr/bin/env bash
# Install-story smoke: stage the tarball, install it, install via the Makefile,
# check both put the same things in the right places, and uninstall. Runs
# against already-built binaries (default target/debug, CPU) so CI can run it
# after the test gate without another build.
#
# ISOLATION: HOME, XDG_*, DESTDIR and every output path are a private temp
# directory, so nothing here can write to a real ~/.local or ~/.config, and no
# service is started, stopped or enabled.
#
#   scripts/install-smoke.sh [--bindir DIR] [--variant cpu|cuda]
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
bindir="$repo/target/debug"
variant=cpu
while [ $# -gt 0 ]; do
    case "$1" in
        --bindir) bindir="$2"; shift 2 ;;
        --variant) variant="$2"; shift 2 ;;
        *) echo "install-smoke: unknown argument '$1'" >&2; exit 2 ;;
    esac
done

fail() { printf 'install-smoke| FAIL: %s\n' "$*" >&2; exit 1; }
say() { printf 'install-smoke| %s\n' "$*"; }
need() { [ -e "$1" ] || fail "missing $1"; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/home/.config" XDG_DATA_HOME="$tmp/home/.local/share" XDG_RUNTIME_DIR="$tmp/run"
mkdir -p "$HOME" "$XDG_RUNTIME_DIR"

# --- 1. the tarball path ---------------------------------------------------
"$repo/scripts/package.sh" --variant "$variant" --bindir "$bindir" --out "$tmp/dist" >/dev/null
tarball="$(ls "$tmp"/dist/*.tar.gz)"
(cd "$tmp/dist" && sha256sum -c "$(basename "$tarball").sha256" >/dev/null) || fail "tarball checksum"
mkdir "$tmp/unpack" && tar -xzf "$tarball" -C "$tmp/unpack"
root="$(ls -d "$tmp"/unpack/*)"
(cd "$root" && sha256sum -c SHA256SUMS >/dev/null) || fail "SHA256SUMS inside the tarball"

dest="$tmp/stage-tar"
DESTDIR="$dest" "$root/install.sh" >/dev/null
bin="$dest$HOME/.local/bin"
need "$bin/dictated"; need "$bin/dictate"
need "$dest$HOME/.local/share/dictate-agent/config.example.toml"
need "$dest$HOME/.local/share/dictate-agent/docs/INSTALL.md"
unit="$dest$HOME/.config/systemd/user/dictated.service"
need "$unit"
grep -qx 'ExecStart=%h/.local/bin/dictated' "$unit" || fail "default unit ExecStart is not %h/.local/bin/dictated: $(grep ExecStart "$unit")"
"$bin/dictate" --version | grep -q '^dictate ' || fail "installed dictate does not run"
"$bin/dictated" --check-config --config "$root/share/dictate-agent/config.example.toml" >/dev/null || fail "installed dictated rejects the example config"

# a non-default prefix must repoint the unit
DESTDIR="$tmp/stage-usr" PREFIX=/usr SYSTEMD_USER_DIR=/usr/lib/systemd/user "$root/install.sh" >/dev/null
grep -qx 'ExecStart=/usr/bin/dictated' "$tmp/stage-usr/usr/lib/systemd/user/dictated.service" || fail "PREFIX=/usr unit ExecStart wrong"
say "tarball install ok"

# --- 2. the Makefile path --------------------------------------------------
mk="$tmp/stage-make"
make -C "$repo" install-files install-unit DESTDIR="$mk" TARGET_DIR="$bindir" >/dev/null
for f in .local/bin/dictated .local/bin/dictate .local/share/dictate-agent/config.example.toml \
    .local/share/dictate-agent/docs/protocol.md .local/share/dictate-agent/docs/INSTALL.md \
    .local/share/dictate-agent/docs/MIGRATION-FROM-PYTHON.md .config/systemd/user/dictated.service; do
    need "$mk$HOME/$f"
    cmp -s "$mk$HOME/$f" "$dest$HOME/$f" || fail "make and install.sh disagree on $f"
done
make -C "$repo" install-files install-unit DESTDIR="$tmp/stage-make-usr" PREFIX=/usr \
    SYSTEMD_USER_DIR=/usr/lib/systemd/user TARGET_DIR="$bindir" >/dev/null
grep -qx 'ExecStart=/usr/bin/dictated' "$tmp/stage-make-usr/usr/lib/systemd/user/dictated.service" || fail "make PREFIX=/usr unit wrong"
say "make install ok"

# --- 3. uninstall leaves user data alone ----------------------------------
mkdir -p "$mk$HOME/.local/share/dictate-agent/models"
echo model >"$mk$HOME/.local/share/dictate-agent/models/ggml-x.bin"
make -C "$repo" uninstall uninstall-unit DESTDIR="$mk" >/dev/null
[ ! -e "$mk$HOME/.local/bin/dictated" ] || fail "uninstall left dictated"
[ ! -e "$mk$HOME/.config/systemd/user/dictated.service" ] || fail "uninstall-unit left the unit"
need "$mk$HOME/.local/share/dictate-agent/models/ggml-x.bin"
DESTDIR="$dest" "$root/install.sh" --uninstall >/dev/null
[ ! -e "$dest$HOME/.local/bin/dictated" ] || fail "install.sh --uninstall left dictated"
say "uninstall ok, models untouched"
say "PASS"
