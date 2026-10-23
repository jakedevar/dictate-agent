#!/usr/bin/env bash
# Hub-side thin SSH glue; no app install, launchd, tunnel or daemon operations.
set -euo pipefail
cd "$(dirname "$0")/../.."
host=${DICTATE_MAC_HOST:-mac-mini}
id=${DICTATE_MAC_BUILD_ID:-${RSI_SESSION_ID:-}}
if [[ -z $id ]]; then
    branch=$(git symbolic-ref --short HEAD)
    [[ $branch == rsi/* ]] || { echo 'set DICTATE_MAC_BUILD_ID outside an RSI sandbox' >&2; exit 2; }
    id=${branch#rsi/}
fi
[[ $id =~ ^[A-Za-z0-9._-]+$ ]] || exit 2
remote="dictate-mac-build/$id.noindex/bench"
ssh -o BatchMode=yes "$host" "mkdir -p ~/$remote/work/synthetic"
rsync -a --exclude work/ --exclude .build/ --exclude .swiftpm/ --exclude results/ mac/bench/ "$host:$remote/"
rsync -a crates/dictated/tests/fixtures/e2e/ "$host:$remote/work/synthetic/"
if [[ ${1:-swift} == whisper ]]; then
    sha256sum -c <(printf '%s  %s\n' 1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69 "$HOME/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin")
    ssh "$host" 'mkdir -p "$HOME/Library/Application Support/dictated/models"; chmod 700 "$HOME/Library/Application Support/dictated/models"'
    # No --delete, and no modification of another worker's model files.
    if ! ssh "$host" 'test -f "$HOME/Library/Application Support/dictated/models/ggml-large-v3-turbo.bin"'; then
        scp -q "$HOME/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin" "$host:Library/Application Support/dictated/models/ggml-large-v3-turbo.bin"
    fi
    ssh "$host" 'shasum -a 256 "$HOME/Library/Application Support/dictated/models/ggml-large-v3-turbo.bin"' | tee /tmp/mac-stt-model-sha.txt
    grep -q '^1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69 ' /tmp/mac-stt-model-sha.txt
fi
ssh -o BatchMode=yes "$host" "cd ~/$remote && bash run-remote.sh ${1:-swift}"
mkdir -p mac/bench/results
rsync -a "$host:$remote/results/" mac/bench/results/
