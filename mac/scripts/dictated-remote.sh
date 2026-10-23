#!/bin/bash
# Build the Rust workspace in a separate, per-session Mac directory. No app,
# TCC, tunnel, hub service or other session's artifacts are touched.
set -euo pipefail
HOST=${DICTATE_MAC_HOST:-mac-mini}
ID=${DICTATE_MAC_BUILD_ID:-${RSI_SESSION_ID:-manual}}
[[ $ID =~ ^[A-Za-z0-9._-]+$ ]] || exit 2
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
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
build) remote 'cargo build --release -p dictated -p dictate-cli --features dictated/metal -j 6' ;;
test) remote 'cargo test -p dictate-server -p dictated -j 6' ;;
workspace) remote 'cargo check --workspace -j 6' ;;
install)
    remote 'cargo build --release -p dictated -p dictate-cli --features dictated/metal -j 6'
    remote 'env DICTATED_BINARY="$PWD/target/release/dictated" DICTATE_CLI="$PWD/target/release/dictate" mac/scripts/dictated-launchagent.sh install'
    ;;
*) echo "usage: $0 setup|build|test|workspace|install" >&2; exit 2 ;;
esac
