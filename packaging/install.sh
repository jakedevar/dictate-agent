#!/bin/sh
# Installer shipped inside the dictate-agent release tarball. It copies files and
# nothing else: it never builds, never starts or stops a service, never touches
# an existing config, model or history database.
#
#   ./install.sh                         install under ~/.local
#   PREFIX=/usr ./install.sh             system-wide (needs root)
#   DESTDIR=/tmp/stage ./install.sh      stage for a packager / test
#   ./install.sh --uninstall             remove what install put there
#
# Environment (same names as the Makefile): PREFIX, BINDIR, DATADIR,
# SYSTEMD_USER_DIR, DESTDIR.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
PREFIX=${PREFIX:-$HOME/.local}
BINDIR=${BINDIR:-$PREFIX/bin}
DATADIR=${DATADIR:-$PREFIX/share/dictate-agent}
APPDIR=${APPDIR:-$PREFIX/share/applications}
ICONDIR=${ICONDIR:-$PREFIX/share/icons/hicolor/256x256/apps}
SYSTEMD_USER_DIR=${SYSTEMD_USER_DIR:-$HOME/.config/systemd/user}
DESTDIR=${DESTDIR:-}

# Under $HOME the unit uses systemd's %h so it stays relocatable.
unit_path() {
    case "$1" in
        "$HOME"/*) printf '%%h/%s' "${1#"$HOME"/}" ;;
        *) printf '%s' "$1" ;;
    esac
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$DESTDIR$BINDIR/dictated" "$DESTDIR$BINDIR/dictate" "$DESTDIR$BINDIR/dictate-ui" \
        "$DESTDIR$SYSTEMD_USER_DIR/dictated.service" \
        "$DESTDIR$APPDIR/dictate-ui.desktop" "$DESTDIR$ICONDIR/dictate-ui.png" \
        "$DESTDIR$DATADIR/config.example.toml" "$DESTDIR$DATADIR/docs/protocol.md" \
        "$DESTDIR$DATADIR/docs/INSTALL.md" "$DESTDIR$DATADIR/docs/MIGRATION-FROM-PYTHON.md"
    rmdir "$DESTDIR$DATADIR/docs" 2>/dev/null || true
    echo "Removed. Your config, models and history were not touched."
    exit 0
fi

[ -x "$here/bin/dictated" ] && [ -x "$here/bin/dictate" ] || {
    echo "install.sh: bin/dictated or bin/dictate missing next to this script" >&2
    exit 1
}

install -d "$DESTDIR$BINDIR" "$DESTDIR$DATADIR/docs" "$DESTDIR$SYSTEMD_USER_DIR"
install -m 0755 "$here/bin/dictated" "$here/bin/dictate" "$DESTDIR$BINDIR/"
install -m 0644 "$here/share/dictate-agent/config.example.toml" "$DESTDIR$DATADIR/"
install -m 0644 "$here"/share/dictate-agent/docs/*.md "$DESTDIR$DATADIR/docs/"

tmp="$DESTDIR$SYSTEMD_USER_DIR/dictated.service.tmp"
sed -e "s|^ExecStart=.*|ExecStart=$(unit_path "$BINDIR")/dictated|" \
    -e "s|^Documentation=.*|Documentation=file://$(unit_path "$DATADIR")/docs/protocol.md|" \
    "$here/systemd/dictated.service" >"$tmp"
install -m 0644 "$tmp" "$DESTDIR$SYSTEMD_USER_DIR/dictated.service"
rm -f "$tmp"

if [ -x "$here/bin/dictate-ui" ]; then
    install -d "$DESTDIR$APPDIR" "$DESTDIR$ICONDIR"
    install -m 0755 "$here/bin/dictate-ui" "$DESTDIR$BINDIR/"
    sed -e "s|^Exec=.*|Exec=$BINDIR/dictate-ui --hub|" "$here/share/applications/dictate-ui.desktop" \
        >"$DESTDIR$APPDIR/dictate-ui.desktop"
    install -m 0644 "$here/share/icons/dictate-ui.png" "$DESTDIR$ICONDIR/dictate-ui.png"
fi

echo "Installed into $DESTDIR$BINDIR."
echo "Next (see docs/INSTALL.md):"
echo "  dictate model pull            # one-time model download"
echo "  systemctl --user daemon-reload && systemctl --user enable --now dictated"
