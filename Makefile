PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
SYSTEMD_USER_DIR ?= $(HOME)/.config/systemd/user

# The daemon and its control client. Both keep their cargo names on disk:
# `dictated` is what the systemd unit starts, `dictate` is what Jake types.
#
# The pre-S02 install target put the daemon at `$(BINDIR)/dictate-agent`. That
# is deliberately left alone rather than overwritten — the Python reference
# daemon and the old unit stay runnable until the v1.0 parity cutover, and
# silently replacing a binary another unit still points at is how you get a
# machine running two daemons that disagree. `make uninstall-legacy` removes it
# when you are ready.
DAEMON_BIN := dictated
CLI_BIN := dictate
LEGACY_BIN := dictate-agent

CARGO_TARGET_DIR ?= target
TARGET_DIR := $(CARGO_TARGET_DIR)/release

# whisper-rs build trap: whisper.cpp is built via CMake, and CUDA's nvcc is
# not on PATH by default on this system (see CLAUDE.md / AGENTS.md).
# WHISPER_DONT_GENERATE_BINDINGS and the CUDA*CXX vars are already set in
# .cargo/config.toml (workspace-wide); PATH is the one thing that must come
# from the invoking shell/Makefile.
export PATH := /opt/cuda/bin:$(PATH)

.PHONY: release test clippy install install-unit clean uninstall uninstall-legacy

release:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo build --release --workspace

test:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo test --workspace

clippy:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo clippy --all-targets --workspace

install: release
	install -d "$(BINDIR)"
	install -m 0755 "$(TARGET_DIR)/$(DAEMON_BIN)" "$(BINDIR)/$(DAEMON_BIN)"
	install -m 0755 "$(TARGET_DIR)/$(CLI_BIN)" "$(BINDIR)/$(CLI_BIN)"
	@echo "Installed $(BINDIR)/$(DAEMON_BIN) and $(BINDIR)/$(CLI_BIN)"
	@echo "Next: make install-unit && systemctl --user enable --now dictated"

install-unit:
	install -d "$(SYSTEMD_USER_DIR)"
	install -m 0644 systemd/dictated.service "$(SYSTEMD_USER_DIR)/dictated.service"
	@echo "Installed $(SYSTEMD_USER_DIR)/dictated.service"
	@echo "Run: systemctl --user daemon-reload && systemctl --user enable --now dictated"

clean:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo clean

uninstall:
	rm -f "$(BINDIR)/$(DAEMON_BIN)" "$(BINDIR)/$(CLI_BIN)"

# Removes the pre-S02 binary. Only do this once you have stopped and disabled
# the old dictate-agent unit.
uninstall-legacy:
	rm -f "$(BINDIR)/$(LEGACY_BIN)"
