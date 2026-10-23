#!/usr/bin/env bash
# Raw STT only: no daemon, microphone, output injection, config or history.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PATH="/opt/cuda/bin:$PATH" CARGO_BUILD_JOBS=8
free=$(nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits | head -1 | tr -d ' ')
[[ $free =~ ^[0-9]+$ && $free -ge 4096 ]] || { echo "need >=4096 MiB free VRAM; got $free" >&2; exit 2; }
printf 'free_vram_mib=%s\n' "$free" > mac/bench/results/hub-environment.txt
nvidia-smi --query-gpu=name,driver_version,memory.total --format=csv >> mac/bench/results/hub-environment.txt
uptime >> mac/bench/results/hub-environment.txt
model="$HOME/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin"
sha256sum -c <(printf '%s  %s\n' 1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69 "$model")
id=${RSI_SESSION_ID:-$(git symbolic-ref --short HEAD)}
id=${id#rsi/}
[[ $id =~ ^[A-Za-z0-9._-]+$ ]] || exit 2
mkdir -p mac/bench/work/samples
rsync -a "${DICTATE_MAC_HOST:-mac-mini}:dictate-mac-build/$id.noindex/bench/work/samples/" mac/bench/work/samples/
cargo build --release --locked --manifest-path mac/bench/whisper/Cargo.toml --features cuda
# Recheck immediately before model allocation; do not start if another user filled VRAM.
free=$(nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits | head -1 | tr -d ' ')
[[ $free -ge 4096 ]] || exit 2
"${CARGO_TARGET_DIR:-target}/release/mac-stt-bench" "$PWD/mac/bench/work/samples/manifest.tsv" "$model" > mac/bench/results/whisper-cuda.jsonl 2> mac/bench/results/whisper-cuda-runtime.log
cargo test --locked --manifest-path mac/bench/score/Cargo.toml
cargo clippy --locked --manifest-path mac/bench/score/Cargo.toml --all-targets -- -D warnings
cargo clippy --locked --manifest-path mac/bench/whisper/Cargo.toml --features cuda --all-targets -- -D warnings
cargo run --quiet --locked --manifest-path mac/bench/score/Cargo.toml -- mac/bench/work/samples/manifest.tsv mac/bench/results/FluidBench.jsonl mac/bench/results/KitBench.jsonl mac/bench/results/whisper-metal.jsonl mac/bench/results/whisper-cuda.jsonl > mac/bench/results/summary.jsonl
