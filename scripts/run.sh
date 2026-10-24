#!/bin/bash
# The retained monolith is opt-in, including after dictated is disabled.
# Never launch a second GPU owner beside the managed service.
if [[ ${DICTATE_ALLOW_LEGACY:-0} != 1 ]]; then
    echo 'Legacy dictate-agent disabled; set DICTATE_ALLOW_LEGACY=1 to opt in.' >&2
    exit 1
fi
if command -v systemctl >/dev/null 2>&1 &&
    systemctl --user is-enabled --quiet dictated 2>/dev/null; then
    exit 0
fi
"$HOME/.local/bin/dictate-gpu-gate" acquire || {
    echo 'dictate-gpu-gate unavailable; refusing ungated legacy start' >&2
    exit 1
}
exec "$HOME/.local/bin/dictate-agent"
