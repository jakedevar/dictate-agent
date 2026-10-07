# Durable home for the whisper-rs/CUDA build incantations that keep getting
# rediscovered — see CLAUDE.md "Build & test" and
# thoughts/shared/handoffs/general/2026-04-10_17-51-32_rust-rewrite-implementation.md
# §Learnings for the underlying why.
#
# WHISPER_DONT_GENERATE_BINDINGS=1 and CUDACXX/CUDAHOSTCXX are already set
# workspace-wide in .cargo/config.toml. The one thing that must come from
# the invoking shell is PATH (CUDA's nvcc isn't on PATH by default here).

export PATH := "/opt/cuda/bin:" + env_var('PATH')

# Build the whole workspace (debug). CPU-only: whisper.cpp's CUDA backend is
# opt-in (`dictated/cuda`), see `release`, `check-cuda` and `e2e`.
build:
    cargo build --workspace

# Run the whole test suite
test:
    cargo test --workspace

# CPU-only tiny-model contract. This is the no-GPU CI fallback; it exercises
# catalog/config/provider seams without downloading a model during unit tests.
test-cpu:
    cargo test -p dictate-stt --no-default-features --features cpu-tiny-ci

# Clippy, all targets, all crates (CPU-only)
clippy:
    cargo clippy --all-targets --workspace

# The WORKER gate: everything a slice must pass, without ever compiling
# whisper.cpp's CUDA backend. Run it instead of the old 3-variant clippy list.
# Workers whose diff touches dictate-stt, the transcription path or the CUDA
# config also run `just check-cuda`.
check-cpu:
    python3 scripts/cutover-test.py
    cargo test --workspace --all-targets
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy -p dictated --features e2e-real --all-targets -- -D warnings
    cargo clippy -p dictate-context --features x11-tests --all-targets -- -D warnings
    cargo fmt --all -- --check
    @! cargo tree -p dictate-cli -e normal | rg 'whisper|dictate-fmt'

# Cutover/rollback integration harness: temporary HOME, stub service manager.
cutover-test:
    python3 scripts/cutover-test.py

# The INTEGRATOR gate (once per integration, with `just e2e`): the CUDA
# variants of clippy. First run compiles whisper.cpp CUDA for sm_120 only.
check-cuda:
    cargo clippy --workspace --all-targets --features dictated/cuda -- -D warnings
    cargo clippy -p dictated --features e2e-real,cuda --all-targets -- -D warnings

# Release build (optimized, LTO, stripped) — produces target/release/dictated
# WITH the CUDA backend (`dictated/cuda`). For a multi-arch build export
# CMAKE_CUDA_ARCHITECTURES first (see .cargo/config.toml).
release:
    cargo build --release --workspace --features dictated/cuda

# Install `dictated` and `dictate` into ~/.local/bin (see Makefile)
install: release
    make install

# Install the systemd user unit for the new daemon (ExecStart follows BINDIR)
install-unit:
    make install-unit

# Optional desktop UI: dictate-ui + desktop entry + icon (needs bun, cargo tauri)
install-ui:
    make install-ui

# Daemon + CLI + unit + UI. Honours PREFIX/BINDIR/DESTDIR/SYSTEMD_USER_DIR, e.g.
#   DESTDIR=$(mktemp -d) just install-all
# See docs/INSTALL.md. Nothing here starts, stops or enables a service.
install-all: install install-unit install-ui

# Stage dist/dictate-agent-<ver>-linux-<arch>-<variant>.tar.gz from built
# binaries (builds nothing, publishes nothing). Variant is `cpu` or `cuda`.
package variant="cpu" *args="":
    scripts/package.sh --variant {{variant}} {{args}}

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
    cargo test --release -p dictated --features e2e-real,cuda --test e2e_real -- --nocapture --test-threads=1 >"$log" 2>&1
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

# S21 live LLM eval against the local Ollama (needs the model installed):
#   just eval-llm                         default ladder, corpus, JSON report
#   just eval-llm gemma4:12b              a specific model
#   just eval-llm gemma4:e4b --record     re-record the CI fixtures
#   just eval-llm "" --latency 30         p50/p95 at 17 and 53 words
#   just eval-llm "" --warmup             cold vs warm vs warm-up benchmarks
#   just eval-llm "" --masks              compare placeholder styles
# Reports go to target/eval/. The real-history tier (aggregates only) needs
# the extra feature: cargo run --release -p dictate-fmt --features
# eval-history --example eval_llm -- --history 500
#
# Live LLM eval (S21): corpus, latency, warm-up, mask sweep, re-record
eval-llm model="" *args="":
    cargo run --release -p dictate-fmt --features eval-live --example eval_llm -- --model "{{model}}" {{args}}

# --- Desktop UI (S32): ui/, a Tauri v2 app outside the cargo workspace -------
# None of these touch the root workspace build; see ui/README.md.

# Install the UI's frontend dependencies from the committed lockfile
ui-deps:
    cd ui && bun install --frozen-lockfile

# Run the UI against the running daemon with a hot-reloading frontend
ui-dev: ui-deps
    cd ui/src-tauri && cargo tauri dev

# Release binary at target/release/dictate-ui (no installer bundle)
ui-build: ui-deps
    cd ui/src-tauri && cargo tauri build --no-bundle

# Frontend typecheck + unit tests, then the Rust bridge/HUD tests and clippy
ui-test: ui-deps
    cd ui && bun run typecheck && bun test
    cd ui/src-tauri && cargo test --all-targets && cargo clippy --all-targets -- -D warnings

# The Flow bar focus/click-through test: i3 inside a private Xvfb, never the
# real display. Needs `just ui-build` and the stub daemon example.
ui-test-x11:
    cd ui/src-tauri && cargo build --example stub_daemon
    ui/tests/x11/hud-focus.sh

# Synthetic-data screenshots of the hub against an isolated real dictated
# (private Xvfb, audio-less, temp paths); writes ui/docs/screenshots/.
# The Flow bar crops come from: ui/tests/x11/hud-focus.sh --shots ui/docs/screenshots
ui-screenshots:
    cargo build -p dictated -p dictate-cli
    ui/tests/x11/screenshots.sh
