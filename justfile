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

# Clippy, all targets, all crates
clippy:
    cargo clippy --all-targets --workspace

# Release build (optimized, LTO, stripped) — produces target/release/dictated
release:
    cargo build --release --workspace

# Install the release binary as ~/.local/bin/dictate-agent (see Makefile)
install: release
    make install
