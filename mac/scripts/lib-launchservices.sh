# shellcheck shell=bash
# LaunchServices helpers shared by install.sh, uninstall.sh and clean-stale.sh
# (#3807). Spotlight, Launchpad and `open` can launch any DictateHub.app that
# LaunchServices knows, so a stale build bundle must be unregistered (and
# deleted), and the installed copy registered. Touches LaunchServices only: never
# TCC, never System Settings.
#
# The DICTATEHUB_* overrides below exist for scripts/test-install-guard.sh only
# (stub HOME, stub destination, recorder commands). Nothing else sets them.
#   DICTATEHUB_DEST_DIR      install directory (default /Applications)
#   DICTATEHUB_LEGACY_DEST   the old per-user bundle (default ~/Applications/DictateHub.app)
#   DICTATEHUB_LSREGISTER    lsregister command
#   DICTATEHUB_OPEN          open command
#   DICTATEHUB_DEFAULTS      defaults command
#   DICTATEHUB_BUILD_ROOT    build root (default ~/dictate-mac-build)

BUNDLE_ID=dev.jakedevar.DictateHub
INSTALL_DIR=${DICTATEHUB_DEST_DIR:-/Applications}
INSTALL_DEST="$INSTALL_DIR/DictateHub.app"
LEGACY_DEST=${DICTATEHUB_LEGACY_DEST:-$HOME/Applications/DictateHub.app}
BUILD_ROOT=${DICTATEHUB_BUILD_ROOT:-$HOME/dictate-mac-build}
LSREGISTER=${DICTATEHUB_LSREGISTER:-/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister}

ls_open() { "${DICTATEHUB_OPEN:-open}" "$@"; }
ls_defaults() { "${DICTATEHUB_DEFAULTS:-defaults}" "$@"; }

# Remove a bundle path from the LaunchServices database. Best effort: a path
# LaunchServices never saw is not an error.
ls_unregister() {
    [[ -x $LSREGISTER ]] || return 0
    "$LSREGISTER" -u "$1" >/dev/null 2>&1 || true
}

# Add a bundle path to the LaunchServices database (-f: even if it looks known).
ls_register() {
    [[ -x $LSREGISTER ]] || return 0
    "$LSREGISTER" -f "$1" >/dev/null 2>&1 || true
}

# True when $1 is a DictateHub app bundle (guards deletes against a wrong path).
is_dictatehub_bundle() {
    [[ -d $1 && $(basename "$1") == DictateHub.app && -f $1/Contents/Info.plist ]] || return 1
    grep -q "<string>$BUNDLE_ID</string>" "$1/Contents/Info.plist"
}
