#!/bin/bash
# On the Mac: remove this build dir's bundle from LaunchServices (#3807), so a
# bundle that `remote.sh run|probe|file-probe|dr-check` launched is never
# offered by Spotlight or Launchpad. The files stay (install copies them).
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib-launchservices.sh
app="$PWD/build/DictateHub.app"
[[ -d $app ]] && ls_unregister "$app"
echo "LaunchServices: unregistered $app"
