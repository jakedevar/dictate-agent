# Durable home for the whisper-rs/CUDA build incantations that keep getting
# rediscovered — see CLAUDE.md "Build & test" and
# thoughts/shared/handoffs/general/2026-04-10_17-51-32_rust-rewrite-implementation.md
# §Learnings for the underlying why.
#
# WHISPER_DONT_GENERATE_BINDINGS=1 and CUDACXX/CUDAHOSTCXX are already set
# workspace-wide in .cargo/config.toml. The one thing that must come from
# the invoking shell is PATH (CUDA's nvcc isn't on PATH by default here).

export PATH := "/opt/cuda/bin:" + env_var('PATH')

# Build the whole workspace (debug)
build:
    cargo build --workspace

# Run the whole test suite
test:
    cargo test --workspace

# CPU-only tiny-model contract. This is the no-GPU CI fallback; it exercises
# catalog/config/provider seams without downloading a model during unit tests.
test-cpu:
    cargo test -p dictate-stt --no-default-features --features cpu-tiny-ci

# Clippy, all targets, all crates
clippy:
    cargo clippy --all-targets --workspace

# Release build (optimized, LTO, stripped) — produces target/release/dictated
release:
    cargo build --release --workspace

# Install `dictated` and `dictate` into ~/.local/bin (see Makefile)
install: release
    make install

# Install the systemd user unit for the new daemon
install-unit:
    make install-unit

# Everything CI checks, in one go
check: test clippy

# Real-hardware end-to-end (S03): real Whisper on CUDA with the installed model,
# real Silero VAD, a real in-process daemon, synthetic-speech fixtures, mock
# injector, audio-less. Needs the GPU and ~/.local/share/dictate-agent/models/
# ggml-large-v3-turbo.bin; never touches the live daemon or the microphone.
# Release profile, because the latency numbers are the point. Prints WER and
# per-stage p50/p95; set E2E_RUNS to change the sample count (default 8).
e2e:
    #!/usr/bin/env bash
    set -uo pipefail
    log="$(mktemp)"
    trap 'rm -f "$log"' EXIT
    cargo test --release -p dictated --features e2e-real --test e2e_real -- --nocapture --test-threads=1 >"$log" 2>&1
    rc=$?
    grep -E '^(e2e\||running |test |test result)' "$log" || true
    if [ "$rc" -ne 0 ]; then
        echo "--- failure detail (whisper.cpp chatter filtered) ---"
        grep -vE '^(whisper_|ggml_|single timestamp|seek =)' "$log" | tail -60
    fi
    exit "$rc"

# Release build -> isolated dictated -> doctor -> transcribe -> assert -> stop.
# Never touches the live daemon, its PID file, config or databases; never opens
# the microphone.
smoke:
    scripts/smoke-real.sh
