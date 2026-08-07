PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
BIN := dictate-agent
CARGO_TARGET_DIR ?= target
TARGET_BIN := $(CARGO_TARGET_DIR)/release/$(BIN)

.PHONY: release install clean uninstall

release:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo build --release

install: release
	install -d "$(BINDIR)"
	install -m 0755 "$(TARGET_BIN)" "$(BINDIR)/$(BIN)"
	@echo "Installed $(BINDIR)/$(BIN)"

clean:
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo clean

uninstall:
	rm -f "$(BINDIR)/$(BIN)"
