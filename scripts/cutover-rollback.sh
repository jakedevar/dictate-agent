#!/usr/bin/env bash
# One-command return to the i3-launched June binary. See migration guide.
source "$(dirname "$0")/cutover-common.sh"
operation=rollback
parse_args "$@"
initialize
rollback
