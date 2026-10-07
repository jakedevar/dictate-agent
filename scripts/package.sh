#!/usr/bin/env bash
# Stage a release tarball from already-built binaries. It builds nothing and
# publishes nothing: the tarball lands in dist/ and what happens next (tagging,
# uploading) is an operator decision.
#
#   scripts/package.sh [--variant cpu|cuda] [--bindir DIR] [--ui PATH] [--out DIR]
#
# --variant  which build the binaries are: `cpu` (cargo build --release) or
#            `cuda` (just release). Default cpu. The staging checks below
#            refuse a binary that does not match its label.
# --bindir   where dictated and dictate are (default target/release)
# --ui       path to dictate-ui to include (default: omitted)
#
# Hard failures (a bad artifact must not ship quietly):
#   * a binary is missing or does not run;
#   * a shared library the binary needs cannot be resolved;
#   * the variant label disagrees with the binary (a `cpu` tarball that links
#     libcuda, or a `cuda` one that does not);
#   * the shipped example config does not load clean under `--check-config`.
# A CUDA build keeps whisper.cpp's CPU backend compiled in, so a cuda tarball
# is a superset of the cpu one and a machine without a GPU still works.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
variant=cpu
bindir="$repo/target/release"
ui=""
out="$repo/dist"

while [ $# -gt 0 ]; do
    case "$1" in
        --variant) variant="$2"; shift 2 ;;
        --bindir) bindir="$2"; shift 2 ;;
        --ui) ui="$2"; shift 2 ;;
        --out) out="$2"; shift 2 ;;
        *) echo "package.sh: unknown argument '$1'" >&2; exit 2 ;;
    esac
done
case "$variant" in cpu | cuda) ;; *) echo "package.sh: --variant must be cpu or cuda" >&2; exit 2 ;; esac

fail() { printf 'package| FAIL: %s\n' "$*" >&2; exit 1; }
say() { printf 'package| %s\n' "$*"; }

version="$(sed -n '/^\[workspace.package\]/,/^$/s/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml")"
[ -n "$version" ] || fail "could not read the workspace version from Cargo.toml"
arch="$(uname -m)"
name="dictate-agent-$version-linux-$arch-$variant"

for b in dictated dictate; do
    [ -x "$bindir/$b" ] || fail "$bindir/$b is missing or not executable (build it first)"
done
[ -z "$ui" ] || [ -x "$ui" ] || fail "--ui $ui is not an executable file"

# --- the artifact checks -------------------------------------------------
bins=("$bindir/dictated" "$bindir/dictate")
[ -z "$ui" ] || bins+=("$ui")
for b in "${bins[@]}"; do
    "$b" --version >/dev/null 2>&1 || [ "$(basename "$b")" = dictate-ui ] || fail "$b does not run"
    if ldd "$b" 2>&1 | grep -q 'not found'; then
        ldd "$b" 2>&1 | grep 'not found' >&2
        fail "$b needs shared libraries this machine cannot resolve"
    fi
done
needed="$(readelf -d "$bindir/dictated" | grep NEEDED || true)"
# whisper.cpp is linked statically; a CUDA build still pulls libcudart/cublas
# dynamically, a CPU build must not.
if printf '%s' "$needed" | grep -qiE 'libcuda|libcudart|libcublas'; then
    [ "$variant" = cuda ] || fail "dictated links CUDA libraries but --variant is cpu (mislabelled artifact)"
else
    [ "$variant" = cpu ] || fail "--variant cuda but dictated links no CUDA library (was it built with --features dictated/cuda?)"
fi
"$bindir/dictated" --check-config --config "$repo/config/config.example.toml" >/dev/null \
    || fail "config/config.example.toml does not load clean under dictated --check-config"

# --- stage ---------------------------------------------------------------
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
root="$stage/$name"
install -d "$root/bin" "$root/systemd" "$root/share/dictate-agent/docs" "$out"
install -m 0755 "$bindir/dictated" "$bindir/dictate" "$root/bin/"
install -m 0644 "$repo/systemd/dictated.service" "$root/systemd/"
install -m 0644 "$repo/config/config.example.toml" "$root/share/dictate-agent/"
install -m 0644 "$repo/docs/protocol.md" "$repo/docs/INSTALL.md" "$repo/docs/MIGRATION-FROM-PYTHON.md" \
    "$root/share/dictate-agent/docs/"
install -m 0755 "$repo/packaging/install.sh" "$root/install.sh"
if [ -n "$ui" ]; then
    install -d "$root/share/applications" "$root/share/icons"
    install -m 0755 "$ui" "$root/bin/dictate-ui"
    install -m 0644 "$repo/packaging/dictate-ui.desktop" "$root/share/applications/"
    install -m 0644 "$repo/ui/src-tauri/icons/icon.png" "$root/share/icons/dictate-ui.png"
fi
(cd "$root" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS)

tar --owner=0 --group=0 --numeric-owner -C "$stage" -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256")
say "wrote $out/$name.tar.gz"
