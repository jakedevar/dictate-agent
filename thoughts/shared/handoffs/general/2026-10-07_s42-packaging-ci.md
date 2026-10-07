# S42 — packaging, CI and install story (handoff)

## Delivered
- `.github/workflows/ci.yml`: ubuntu-24.04, toolchain pinned 1.94.1; job
  `check-cpu` = `just check-cpu` + `scripts/install-smoke.sh` + a grep that the
  workflow holds no secrets/publish steps; job `ui` = `just ui-test`
  (webkit2gtk-4.1 etc.). Read-only token, no GPU/secrets/models/fixtures.
- `Makefile`: `DESTDIR`, `DATADIR`, `TARGET_DIR`; new `install-files`
  (no build), `install-ui`/`install-ui-files`, `install-all`, `release-cpu`,
  `ui-release`, `uninstall-unit`/`uninstall-ui`. `install-unit` rewrites
  `ExecStart`/`Documentation` to the real `BINDIR`/`DATADIR` (`%h` under $HOME).
  `uninstall` removes listed files only (DATADIR also holds models/history).
- `justfile`: `install-ui`, `install-all`, `package <variant>`.
- `scripts/package.sh` (tarball + sha256, hard-fails on unresolved libs,
  cpu/cuda label mismatch, bad example config), `packaging/install.sh`
  (shipped in tarball, `--uninstall`), `scripts/install-smoke.sh` (private HOME),
  `packaging/aur/PKGBUILD` (`dictate-agent-git`, `_cuda=1`), desktop entry.
- Docs: `docs/INSTALL.md` (deps, install, first run, upgrade, rollback),
  `docs/MIGRATION-FROM-PYTHON.md` (cutover + rollback, documented only),
  README.md (was empty), CLAUDE.md pointer.

## Deviations from the slice map
- No macOS job and no cargo-dist: macOS is Wave 4 (S40); tarball is hand-rolled.
- No `$ORIGIN` rpath / co-located whisper lib: whisper.cpp links statically;
  the R1 intent became the staging checks (ldd unresolved, CUDA-label match).
- No LAN-API guide: S33 (`dictate-server`) is in flight; add it there.
- PKGBUILD `license=('custom')`: the repo has no LICENSE file (UI crate says
  MIT). Needs Jake before anything is published.
- Single Ubuntu runner rather than an Arch container (apt names unverifiable
  locally; Arch names were checked via pacman). CI itself has not run: it can
  only run after a push, which is out of scope.

## Verified here
`just check-cpu` green (979 tests, 0 failed); `just ui-test` green;
`scripts/install-smoke.sh` PASS; negative cases (cuda-labelled CPU binary,
missing bindir) fail hard; CPU whisper-rs-sys builds with CUDACXX/CUDAHOSTCXX
pointed at nonexistent paths. Never touched real ~/.local or ~/.config.
