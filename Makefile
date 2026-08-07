PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
BIN := dictate-agent
# Cargo package/binary name (crates/dictated) — kept distinct from the
# installed name above so scripts/run.sh, the systemd unit, and Jake's
# muscle memory (`dictate-agent`) don't need to change with the S00
# workspace split.
CARGO_BIN := dictated
CARGO_TARGET_DIR ?= target
TARGET_BIN := $(CARGO_TARGET_DIR)/release/$(CARGO_BIN)

# whisper-rs build trap: whisper.cpp is built via CMake, and CUDA's nvcc is
# not on PATH by default on this system (see CLAUDE.md / AGENTS.md).
# WHISPER_DONT_GENERATE_BINDINGS and the CUDA*CXX vars are already set in
# .cargo/config.toml (workspace-wide); PATH is the one thing that must come
# from the invoking shell/Makefile.
export PATH := /opt/cuda/bin:$(PATH)

.PHONY: release test clippy install clean uninstall

release:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo build --release --workspace

test:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo test --workspace

clippy:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo clippy --all-targets --workspace

install: release
	install -d "$(BINDIR)"
	install -m 0755 "$(TARGET_BIN)" "$(BINDIR)/$(BIN)"
	@echo "Installed $(BINDIR)/$(BIN) (built as crates/dictated, binary 'dictated')"

clean:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo clean

uninstall:
	rm -f "$(BINDIR)/$(BIN)"
