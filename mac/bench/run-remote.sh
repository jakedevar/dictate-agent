#!/usr/bin/env bash
# Invoked on mac-mini, only inside this session's isolated work directory.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="/opt/homebrew/bin:$PATH"
export CARGO_HOME="$PWD/work/cargo" RUSTUP_HOME="$PWD/work/rustup"
export CARGO_TARGET_DIR="$PWD/work/target" CARGO_BUILD_JOBS=6
export XDG_CACHE_HOME="$PWD/work/cache"
export WHISPER_DONT_GENERATE_BINDINGS=1
mkdir -p work results
models="$HOME/Library/Application Support/dictated/models"
mkdir -p "$models"
chmod 700 "$models"
phase=${1:-swift}
spm() { nice -n 10 swift "$@" --cache-path "$PWD/work/spm-cache" --config-path "$PWD/work/spm-config" --security-path "$PWD/work/spm-security"; }
case "$phase" in
swift)
    spm build -c release -j 6 --product Prepare
    if [[ ! -f work/test-clean.tar.gz ]]; then
        curl -fL --retry 3 https://www.openslr.org/resources/12/test-clean.tar.gz -o work/test-clean.tar.gz
    fi
    shasum -a 256 work/test-clean.tar.gz > results/corpus.sha256
    if [[ ! -d work/LibriSpeech ]]; then tar -xzf work/test-clean.tar.gz -C work; fi
    mkdir -p work/samples
    .build/release/Prepare work/LibriSpeech/test-clean work/samples > work/samples/manifest.tsv
    for name in short medium long; do
        cp "work/synthetic/$name.wav" "work/synthetic/$name.txt" work/samples/
        printf '%s\tsynthetic\t%s.wav\t%s.txt\n' "$name" "$name" "$name" >> work/samples/manifest.tsv
    done
    cp work/samples/manifest.tsv results/manifest.tsv
    (cd work/samples && shasum -a 256 *.wav *.txt) > results/samples.sha256
    failures=0
    for product in FluidBench KitBench; do
        # A candidate's failure must not discard the other candidate's evidence.
        if spm build -c release -j 6 --product "$product" > "results/$product-build.log" 2>&1; then
            if nice -n 10 ".build/release/$product" "$PWD/work/samples/manifest.tsv" "$models" prepare > "results/$product-prepare.log" 2>&1; then
                if ! /usr/bin/time -l nice -n 10 ".build/release/$product" "$PWD/work/samples/manifest.tsv" "$models" > "results/$product.jsonl" 2> "results/$product-runtime.log"; then
                    echo "failed $product run" >&2; failures=$((failures + 1))
                fi
            else echo "failed $product preparation" >&2; failures=$((failures + 1)); fi
        else echo "failed $product build" >&2; failures=$((failures + 1)); fi
    done
    cp Package.resolved results/Package.resolved
    [[ $failures -eq 0 ]]
    ;;
whisper)
    if [[ ! -x "$CARGO_HOME/bin/cargo" ]]; then
        curl -fL --retry 3 https://static.rust-lang.org/rustup/dist/aarch64-apple-darwin/rustup-init -o work/rustup-init
        chmod +x work/rustup-init
        work/rustup-init -y --profile minimal --no-modify-path --default-toolchain stable
    fi
    if ! command -v cmake >/dev/null; then
        url=$(curl -fsSL https://api.github.com/repos/Kitware/CMake/releases/latest | /usr/bin/plutil -extract assets json -o - - | /usr/bin/grep -o 'https:[^"]*macos-universal.tar.gz' | /usr/bin/head -1)
        curl -fL --retry 3 "$url" -o work/cmake.tar.gz
        mkdir -p work/cmake
        tar -xzf work/cmake.tar.gz --strip-components=1 -C work/cmake
        export PATH="$PWD/work/cmake/CMake.app/Contents/bin:$PATH"
    fi
    nice -n 10 "$CARGO_HOME/bin/cargo" build --release --locked --manifest-path whisper/Cargo.toml --features metal
    /usr/bin/time -l nice -n 10 "$CARGO_TARGET_DIR/release/mac-stt-bench" "$PWD/work/samples/manifest.tsv" "$models/ggml-large-v3-turbo.bin" > results/whisper-metal.jsonl 2> results/whisper-metal-runtime.log
    cp whisper/Cargo.lock results/whisper-Cargo.lock
    ;;
verify)
    mkdir -p results/validation
    spm build -c release -j 6 --product FluidBench
    /usr/bin/time -l nice -n 10 .build/release/FluidBench "$PWD/work/samples/manifest.tsv" "$models" > results/validation/FluidBench.jsonl 2> results/validation/FluidBench-runtime.log
    nice -n 10 "$CARGO_HOME/bin/cargo" build --release --locked --manifest-path whisper/Cargo.toml --features metal
    /usr/bin/time -l nice -n 10 "$CARGO_TARGET_DIR/release/mac-stt-bench" "$PWD/work/samples/manifest.tsv" "$models/ggml-large-v3-turbo.bin" > results/validation/whisper-metal.jsonl 2> results/validation/whisper-metal-runtime.log
    bash "$0" hashes
    ;;
hashes)
    (cd "$models" && find . -type f ! -path '*/.cache/*' ! -name '.*' -exec shasum -a 256 {} \; | LC_ALL=C sort -k2) > results/models.sha256
    ;;
*) echo "usage: $0 swift|whisper|verify|hashes" >&2; exit 2 ;;
esac
