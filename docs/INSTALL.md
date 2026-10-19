# Installing dictate-agent

One path, three pieces: the daemon `dictated` and its CLI `dictate`, the systemd
user unit that runs the daemon, and (optionally) the desktop UI `dictate-ui`.
The UI is a client of the daemon; the daemon works without it.

Supported today: **Linux on X11** (Wayland injection is a stub). macOS and
Windows are later slices. Nothing here is a published release: tagging and
uploading artifacts is an operator decision, so the supported ways to get
binaries are building from source or the tarball `scripts/package.sh` stages.

## 1. Dependencies

Build time (the first build compiles whisper.cpp with CMake, a few minutes):

| | Arch | Debian/Ubuntu |
|---|---|---|
| toolchain | `rust cmake clang pkgconf base-devel` | `rustup cmake clang pkg-config build-essential` |
| audio (cpal) | `alsa-lib` | `libasound2-dev` |
| TLS (ONNX download in the VAD build, model pull) | `openssl` | `libssl-dev` |
| `just` recipes | `just ripgrep` | `just ripgrep` |
| UI only | `webkit2gtk-4.1 gtk3 libayatana-appindicator librsvg bun` and `cargo install tauri-cli --version '^2'` | `libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`, bun from bun.sh |
| CUDA build only | `cuda` (`/opt/cuda/bin` on `PATH`) | NVIDIA's CUDA toolkit |

Runtime: PipeWire or PulseAudio for the microphone, an X11 session for text
injection, and for hold-to-talk hotkeys your user must be in the `input` group (`sudo usermod -aG input $USER`,
then log in again; without it hotkeys degrade to your window manager's
`dictate toggle` binding).

## 2. Build and install

```bash
git clone https://github.com/jakedevar/dictate-agent && cd dictate-agent

just release          # CUDA build (NVIDIA GPU, toolkit required). Cold: ~3 min for CUDA.
# or, on a machine with no GPU:
cargo build --release --workspace     # CPU-only; `dictate status` reports backend: cpu

just install          # release + install dictated and dictate into ~/.local/bin
just install-unit     # ~/.config/systemd/user/dictated.service
just install-ui       # optional: dictate-ui, desktop entry, icon
```

`just install` and `make install` read these variables (all optional):

| Variable | Default | Meaning |
|---|---|---|
| `PREFIX` | `~/.local` | root for `bin/` and `share/` |
| `BINDIR` | `$PREFIX/bin` | where `dictated`, `dictate`, `dictate-ui` go |
| `DATADIR` | `$PREFIX/share/dictate-agent` | example config and docs (never models or history) |
| `SYSTEMD_USER_DIR` | `~/.config/systemd/user` | where the unit goes (`/usr/lib/systemd/user` for packages) |
| `DESTDIR` | empty | stage root for packagers and tests |

`install-unit` rewrites `ExecStart` to the actual `BINDIR`, so a non-default
prefix still starts the right binary. To install files without building:
`make install-files install-unit TARGET_DIR=target/release`. To preview an
install without touching your home directory:

```bash
make install-files install-unit DESTDIR=$(mktemp -d) PREFIX=/usr SYSTEMD_USER_DIR=/usr/lib/systemd/user
```

### From the tarball

`scripts/package.sh --variant cpu|cuda [--ui target/release/dictate-ui]` stages
`dist/dictate-agent-<version>-linux-<arch>-<variant>.tar.gz` (+ `.sha256`) from
already-built binaries. It fails hard when a binary does not run, has an
unresolvable shared library, links CUDA while labelled `cpu` (or the reverse),
or when the example config does not load clean. The tarball contains
`install.sh`, which honours the same variables and has `--uninstall`. A CUDA
build keeps whisper.cpp's CPU backend compiled in, so the `cuda` tarball still
runs on a machine without a GPU.

### Arch Linux

`packaging/aur/PKGBUILD` builds `dictate-agent-git` (CPU build; set
`_cuda=1` before `makepkg` for the CUDA backend). It is not published to the AUR.

## 3. First run

```bash
dictate model pull              # large-v3-turbo into ~/.local/share/dictate-agent/models, SHA-256 verified
dictate model pull tiny.en      # a small model for a slow machine or a first try
cp ~/.local/share/dictate-agent/config.example.toml ~/.config/dictate-agent/config.toml   # only if you have none
dictated --check-config         # effective config + every warning; exit 1 if invalid
systemctl --user daemon-reload
systemctl --user enable --now dictated
dictate doctor                  # one check per dependency, each with a one-line fix
dictate toggle                  # speak, then run it again to stop and type
```

If `~/.config/dictate-agent/config.toml` already exists (it is shared with the
older daemons) do not overwrite it: `dictated` reads the Python-era shape and
warns once per unknown key. The model download is the only network access the
daemon makes by itself, and only when you run `dictate model pull`.

Bind `dictate toggle` and `dictate cancel` in your window manager, or enable
`[hotkey]` in the config for evdev hold-to-talk.

The UI: put `dictate-ui` where your session starts it, for example in i3
`exec --no-startup-id ~/.local/bin/dictate-ui`. See `ui/README.md`.

For i3 sessions, also start the enabled daemon at desktop login. The systemd
user manager can outlive an X11 session, so its `default.target` does not
necessarily start again when i3 does:

```i3
exec --no-startup-id systemctl --user start dictated
```

`scripts/run.sh` also starts the service when it is enabled; after rollback
disables it, that launcher starts the legacy binary. The unit restarts after
unexpected clean exits as well as failures. `systemctl --user stop dictated`
still stops it until the next explicit start.

### Diagnosing startup and missing cleanup

```bash
systemctl --user status dictated
systemctl --user cat dictated
journalctl --user -u dictated -b --no-pager -n 60
dictate status
dictate doctor --quick
ollama list
```

An enabled unit can be inactive. The CLI and i3 bindings need `dictated`;
running the older `dictate-agent` binary does not provide its control socket.
Use the cutover procedure below to move between them, keeping one daemon in
charge.

Check the unit's complete `ExecStart`, including drop-ins. `cutover.sh` pins
a generated `cutover/.../steady.toml`, so editing the usual
`~/.config/dictate-agent/config.toml` does not change that service's config.
Pass the effective path to `dictated --check-config --config PATH` and restart
the service after changing it. To return to the normal path, back up both
files, copy the desired settings there, and change the cutover drop-in's
`--config` argument before `daemon-reload` and restart.

Cleanup fails open when its configured model is absent. Check
`dictate status --json` for formatter health, compare `[format.llm].models`
(or the older `[grammar].model`) with `ollama list`, and choose an installed
model. Raw dictation can still work while cleanup is failing.

If Ghostty sessions report `paste unconfirmed; no typing retry`, the existing
per-app typing policy bypasses the clipboard transaction from the outset:

```toml
[[context.profiles]]
name = "ghostty-typing"
match = { class = "com.mitchellh.ghostty" }
category = "terminal"
inject = "type"
```

Put this in the effective daemon config and restart it; first matching profile
wins. Check `dictate context --json` while Ghostty has focus. This is a
workaround for clipboard compatibility, not a retry after an uncertain paste.

## 4. Upgrade

```bash
git pull
just install          # or install from a new tarball: ./install.sh
systemctl --user restart dictated
dictate status && dictate doctor
```

Upgrades replace the binaries and the unit only. Config, models and
`history.db` live outside the install paths and are never touched; history
migrations run on the daemon's first start and are additive.

## 5. Roll back

Before upgrading, keep what you can return to:

```bash
cp -a ~/.local/bin/dictated ~/.local/bin/dictated.prev
cp -a ~/.local/bin/dictate  ~/.local/bin/dictate.prev
```

To roll back: `systemctl --user stop dictated`, move the `.prev` files back,
`systemctl --user start dictated`. Or rebuild an older commit (`git checkout
<tag>` then `just install`) or install an older tarball. The history database
is forward-compatible for reads by an older daemon only when the newer
migrations were additive; if `dictated` refuses an old schema, restore the
`history.db` copy you took (`cp ~/.local/share/dictated/history.db{,.bak}`
before upgrading is cheap).

`make uninstall uninstall-unit uninstall-ui` removes only what the install put
down (`DATADIR` is never removed wholesale because models live beside it).

## 6. Switching from the Python or monolithic daemon

See [MIGRATION-FROM-PYTHON.md](MIGRATION-FROM-PYTHON.md). `just install` is
safe to run beside the old daemon: it uses different binary names, socket, PID
file and database, and never stops or replaces anything running.

## CI

`.github/workflows/ci.yml` runs on a plain GitHub-hosted Linux runner with no
GPU, no secrets and no model files: `just check-cpu` (tests, clippy, rustfmt,
CLI dependency check), the UI's typecheck/bun/cargo tests, and a packaging
smoke that stages the CPU tarball and installs it into a temp directory. It
never publishes anything.
