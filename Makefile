PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share/dictate-agent
APPDIR ?= $(PREFIX)/share/applications
ICONDIR ?= $(PREFIX)/share/icons/hicolor/256x256/apps
SYSTEMD_USER_DIR ?= $(HOME)/.config/systemd/user

# Packagers stage into DESTDIR (`make install DESTDIR=$pkgdir PREFIX=/usr
# SYSTEMD_USER_DIR=/usr/lib/systemd/user`). Every path above is prefixed with it
# at install time and never baked into a file: the systemd unit keeps the final
# runtime path. Tests for these targets must set DESTDIR (or PREFIX and
# SYSTEMD_USER_DIR) to a temp directory, never the real ~/.local.
DESTDIR ?=

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
UI_BIN := dictate-ui
LEGACY_BIN := dictate-agent

CARGO_TARGET_DIR ?= target
# Where the built binaries are read from. `make install-files TARGET_DIR=
# target/debug` installs a debug build (used to test the install targets).
TARGET_DIR ?= $(CARGO_TARGET_DIR)/release

# The unit points at the path it was installed under, so a PREFIX other than
# ~/.local still starts the right binary. Under $HOME the path is written with
# systemd's %h specifier, which keeps the unit relocatable across machines.
unit_path = $(if $(filter $(HOME)/%,$(1)),%h/$(patsubst $(HOME)/%,%,$(1)),$(1))

# whisper-rs build trap: whisper.cpp is built via CMake, and CUDA's nvcc is
# not on PATH by default on this system (see CLAUDE.md / AGENTS.md).
# WHISPER_DONT_GENERATE_BINDINGS and the CUDA*CXX vars are already set in
# .cargo/config.toml (workspace-wide); PATH is the one thing that must come
# from the invoking shell/Makefile.
export PATH := /opt/cuda/bin:$(PATH)

.PHONY: release release-cpu ui-release test clippy install install-files \
	install-unit install-ui install-ui-files install-all uninstall \
	uninstall-unit uninstall-ui uninstall-legacy clean

release:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo build --release --workspace --features dictated/cuda

# CPU-only daemon (no CUDA toolkit needed): what CI packages and what an AUR or
# distro build without a GPU should use.
release-cpu:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo build --release --workspace

# The desktop UI is its own Tauri workspace (ui/src-tauri). Needs bun and
# `cargo tauri`; see ui/README.md.
ui-release:
	cd ui && bun install --frozen-lockfile
	cd ui/src-tauri && CARGO_TARGET_DIR="$(abspath $(CARGO_TARGET_DIR))" cargo tauri build --no-bundle

test:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo test --workspace

clippy:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo clippy --all-targets --workspace

# Build (CUDA) then install. `install-files` is the same minus the build, which
# is what packagers and the tests use.
install: release install-files

install-files:
	install -d "$(DESTDIR)$(BINDIR)" "$(DESTDIR)$(DATADIR)/docs"
	install -m 0755 "$(TARGET_DIR)/$(DAEMON_BIN)" "$(DESTDIR)$(BINDIR)/$(DAEMON_BIN)"
	install -m 0755 "$(TARGET_DIR)/$(CLI_BIN)" "$(DESTDIR)$(BINDIR)/$(CLI_BIN)"
	install -m 0644 config/config.example.toml "$(DESTDIR)$(DATADIR)/config.example.toml"
	install -m 0644 docs/protocol.md docs/INSTALL.md docs/MIGRATION-FROM-PYTHON.md "$(DESTDIR)$(DATADIR)/docs/"
	@echo "Installed $(DESTDIR)$(BINDIR)/$(DAEMON_BIN) and $(DESTDIR)$(BINDIR)/$(CLI_BIN)"
	@echo "Next: make install-unit && systemctl --user enable --now dictated"

# The unit is rewritten on the way in so ExecStart and Documentation follow
# BINDIR/DATADIR. It never touches the Python daemon's dictate-agent.service.
install-unit:
	install -d "$(DESTDIR)$(SYSTEMD_USER_DIR)"
	sed -e 's|^ExecStart=.*|ExecStart=$(call unit_path,$(BINDIR))/$(DAEMON_BIN)|' \
	    -e 's|^Documentation=.*|Documentation=file://$(call unit_path,$(DATADIR))/docs/protocol.md|' \
	    systemd/dictated.service > "$(DESTDIR)$(SYSTEMD_USER_DIR)/dictated.service.tmp"
	install -m 0644 "$(DESTDIR)$(SYSTEMD_USER_DIR)/dictated.service.tmp" "$(DESTDIR)$(SYSTEMD_USER_DIR)/dictated.service"
	rm -f "$(DESTDIR)$(SYSTEMD_USER_DIR)/dictated.service.tmp"
	@echo "Installed $(DESTDIR)$(SYSTEMD_USER_DIR)/dictated.service"
	@echo "Run: systemctl --user daemon-reload && systemctl --user enable --now dictated"

# The optional desktop UI: `dictate-ui` plus a desktop entry and icon.
install-ui: ui-release install-ui-files

install-ui-files:
	install -d "$(DESTDIR)$(BINDIR)" "$(DESTDIR)$(APPDIR)" "$(DESTDIR)$(ICONDIR)"
	install -m 0755 "$(TARGET_DIR)/$(UI_BIN)" "$(DESTDIR)$(BINDIR)/$(UI_BIN)"
	sed -e 's|^Exec=.*|Exec=$(BINDIR)/$(UI_BIN) --hub|' packaging/dictate-ui.desktop \
	    > "$(DESTDIR)$(APPDIR)/dictate-ui.desktop.tmp"
	install -m 0644 "$(DESTDIR)$(APPDIR)/dictate-ui.desktop.tmp" "$(DESTDIR)$(APPDIR)/dictate-ui.desktop"
	rm -f "$(DESTDIR)$(APPDIR)/dictate-ui.desktop.tmp"
	install -m 0644 ui/src-tauri/icons/icon.png "$(DESTDIR)$(ICONDIR)/dictate-ui.png"
	@echo "Installed $(DESTDIR)$(BINDIR)/$(UI_BIN)"

install-all: install install-unit install-ui

clean:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo clean

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/$(DAEMON_BIN)" "$(DESTDIR)$(BINDIR)/$(CLI_BIN)"
	# DATADIR is ~/.local/share/dictate-agent by default, which also holds the
	# models and history.db: remove only the files install-files put there,
	# never the directory.
	rm -f "$(DESTDIR)$(DATADIR)/config.example.toml" \
	      "$(DESTDIR)$(DATADIR)/docs/protocol.md" "$(DESTDIR)$(DATADIR)/docs/INSTALL.md" \
	      "$(DESTDIR)$(DATADIR)/docs/MIGRATION-FROM-PYTHON.md"
	-rmdir "$(DESTDIR)$(DATADIR)/docs" 2>/dev/null

uninstall-unit:
	rm -f "$(DESTDIR)$(SYSTEMD_USER_DIR)/dictated.service"

uninstall-ui:
	rm -f "$(DESTDIR)$(BINDIR)/$(UI_BIN)" "$(DESTDIR)$(APPDIR)/dictate-ui.desktop" "$(DESTDIR)$(ICONDIR)/dictate-ui.png"

# Removes the pre-S02 binary. Only do this once you have stopped and disabled
# the old dictate-agent unit.
uninstall-legacy:
	rm -f "$(DESTDIR)$(BINDIR)/$(LEGACY_BIN)"
