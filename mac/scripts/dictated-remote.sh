#!/bin/bash
# Build the Rust workspace in a separate, per-session Mac directory. No app,
# TCC, tunnel, hub service or other session's artifacts are touched.
set -euo pipefail
HOST=${DICTATE_MAC_HOST:-mac-mini}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
# Durable jobs intentionally have a smaller environment than agent turns.
# The RSI branch is durable custody, so it is the fallback session identity.
branch=$(git -C "$ROOT" branch --show-current)
case $branch in rsi/*) default_id=${branch#rsi/};; *) default_id=manual;; esac
ID=${DICTATE_MAC_BUILD_ID:-${RSI_SESSION_ID:-$default_id}}
[[ $ID =~ ^[A-Za-z0-9._-]+$ ]] || exit 2
DIR="dictate-mac-build/$ID.noindex/rust"
ssh_mac() { ssh -o BatchMode=yes -o ConnectTimeout=15 "$HOST" "$@"; }
remote() { ssh_mac "cd ~/$DIR && export PATH=\$HOME/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin && export CARGO_BUILD_JOBS=6 && nice -n 10 $*"; }
ssh_mac "mkdir -p ~/$DIR && touch ~/${DIR%/rust}/.metadata_never_index"
rsync -a --delete --exclude .git --exclude target --exclude .build --exclude build \
    --exclude ui/node_modules --exclude .rsi --exclude thoughts "$ROOT/" "$HOST:$DIR/"
case ${1:-} in
setup)
    ssh_mac 'export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"; if ! command -v cargo >/dev/null; then mkdir -p "$HOME/.cargo"; curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs -o "$HOME/.cargo/rustup-init.sh" && sh "$HOME/.cargo/rustup-init.sh" -y --profile minimal --no-modify-path; fi; command -v cmake >/dev/null || brew install cmake'
    ;;
build) remote 'swift build --package-path mac/parakeet -c release --jobs 6'
    remote 'cargo build --release --workspace --features dictated/metal -j 6' ;;
parakeet-test) remote 'swift test --package-path mac/parakeet --jobs 6'
    remote 'swift build --package-path mac/parakeet -c release --jobs 6' ;;
test) remote 'swift test --package-path mac/parakeet --jobs 6'
    remote 'cargo test -p dictate-stt -p dictate-server -p dictated -j 6'
    remote 'mac/scripts/test-dictated-launchagent.sh' ;;
workspace) remote 'cargo check --workspace -j 6' ;;
smoke) remote mac/scripts/dictated-smoke.sh ;;
parakeet-bench)
    remote 'swift build --package-path mac/parakeet -c release --jobs 6'
    remote 'mkdir -p build/parakeet-results && mac/parakeet/.build/release/UdsBench build/parakeet-fixtures/manifest.tsv > build/parakeet-results/uds.jsonl'
    mkdir -p "$ROOT/mac/parakeet/results"
    rsync -a "$HOST:$DIR/build/parakeet-results/" "$ROOT/mac/parakeet/results/"
    ;;
install)
    remote 'swift build --package-path mac/parakeet -c release --jobs 6'
    remote 'cargo build --release --workspace --features dictated/metal -j 6'
    remote 'env DICTATED_BINARY="$PWD/target/release/dictated" DICTATE_CLI="$PWD/target/release/dictate" DICTATE_PARAKEET_BINARY="$PWD/mac/parakeet/.build/release/dictate-parakeet" mac/scripts/dictated-launchagent.sh install'
    ;;
*) echo "usage: $0 setup|build|test|parakeet-test|parakeet-bench|workspace|install|smoke" >&2; exit 2 ;;
esac
