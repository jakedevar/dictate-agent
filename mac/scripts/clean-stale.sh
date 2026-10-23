#!/bin/bash
# On the Mac: unregister from LaunchServices and delete every stale DictateHub
# build bundle under ~/dictate-mac-build/, keeping the bundle of the current
# build id (#3807). A stale ad-hoc build bundle can be launched from Spotlight or
# Launchpad, and macOS then attaches privacy grants to the wrong bundle.
#
#   scripts/clean-stale.sh BUILD_ID      # keep ~/dictate-mac-build/BUILD_ID.noindex
#   scripts/clean-stale.sh --dry-run BUILD_ID
#
# Only bundles named DictateHub.app with this bundle id, physically under the
# build root, are touched: never /Applications, ~/Applications or anything
# outside ~/dictate-mac-build/. Build dirs themselves stay (only the .app goes).
# It also unregisters paths LaunchServices still lists under the build root
# whose bundle is already gone. Never touches TCC.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib-launchservices.sh

dry=0
if [[ ${1:-} == --dry-run ]]; then dry=1; shift; fi
id=${1:-}
[[ $id =~ ^[A-Za-z0-9._-]+$ ]] || { echo "usage: $0 [--dry-run] BUILD_ID" >&2; exit 2; }
[[ -d $BUILD_ROOT ]] || { echo "no $BUILD_ROOT; nothing to clean"; exit 0; }
root=$(cd "$BUILD_ROOT" && pwd -P)
keep="$root/$id.noindex"

removed=0
kept=0
while IFS= read -r app; do
    [[ -n $app ]] || continue
    [[ -L $app ]] && continue
    real=$(cd "$(dirname "$app")" && pwd -P)/$(basename "$app")
    case $real in "$root"/*) ;; *) continue ;; esac
    is_dictatehub_bundle "$real" || continue
    case $real in
    "$keep"/*) kept=$((kept + 1)); echo "keep   $real"; continue ;;
    esac
    echo "remove $real"
    if [[ $dry == 0 ]]; then
        ls_unregister "$real"
        rm -rf "$real"
    fi
    removed=$((removed + 1))
done < <(find "$root" -maxdepth 6 -name DictateHub.app -type d -prune 2>/dev/null | sort)

# Registered paths under the build root whose bundle no longer exists.
ghosts=0
if [[ -x $LSREGISTER ]]; then
    while IFS= read -r path; do
        [[ -n $path && ! -e $path ]] || continue
        case $path in "$root"/*) ;; *) continue ;; esac
        echo "unregister (gone) $path"
        [[ $dry == 1 ]] || ls_unregister "$path"
        ghosts=$((ghosts + 1))
    done < <("$LSREGISTER" -dump 2>/dev/null | sed -n 's/^path: *\(.*DictateHub\.app\) (0x[0-9a-f]*)$/\1/p' | sort -u)
fi

: > "$root/.metadata_never_index"
echo "CLEAN-STALE removed=$removed kept=$kept ghosts=$ghosts dry_run=$dry"
