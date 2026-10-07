# Build cost: CUDA opt-in, sm_120 pin, CPU worker gate (Issue #1437)

## Design
- `dictate-stt`: `default = ["transcribe"]` (was `cuda`). `cuda` stays an opt-in
  feature (`transcribe` + `whisper-rs/cuda`).
- `dictate-core`: no longer forces `dictate-stt/cuda`; new feature
  `cuda = ["dictate-stt/cuda"]`.
- `dictated`: new feature `cuda = ["dictate-core/cuda"]`, not default.
- Chain: `dictated/cuda` -> `dictate-core/cuda` -> `dictate-stt/cuda`. `--workspace`
  with no flags is CPU-only; `just release|install`, `make release|install` and
  `just e2e` pass the feature. `e2e-real` is deliberately separate from `cuda`, so
  the `e2e-real` clippy line stays in the CPU gate.
- Rejected: default-on `cuda` for `dictated` (Cargo unifies member defaults, so
  `--workspace` would pull CUDA again).
- Cost of the choice: a plain `cargo build --release` gives a CPU `dictated`.
  Documented in CLAUDE.md; `just release` is the supported path.
- `.cargo/config.toml`: `CMAKE_CUDA_ARCHITECTURES=120a-real`. whisper-rs-sys forwards
  every `CMAKE_*` env var. Verified in `CMakeCache.txt` and in ggml-cuda's
  `flags.make` (only `--generate-code=arch=compute_120a,code=[sm_120a]`).
  A shell value overrides it (multi-arch release recipe in CLAUDE.md).
- Gates: `just check-cpu` (worker), `just check-cuda` (integrator) + `just e2e`.

## Measurements (cold fresh CARGO_TARGET_DIR, `-j 8`; `/usr/bin/time` is not installed here, bash `time` used: real/user/sys)
Command for CUDA builds: `cargo build -j 8 -p dictate-stt --no-default-features --features cuda`
(whisper-rs-sys CUDA plus a few small crates).

| build | load (1m) | wall | user | sys |
|---|---|---|---|---|
| before (no pin; GPU visible, ggml chooses `native` = sm_120a) | 21.4 | 155 s | 1039 s | 55 s |
| after (pinned 120a-real) | 10.1 | 150 s | 1015 s | 54 s |
| before, GPU-less equivalent (`GGML_NATIVE=OFF`, no pin: full arch list) | 22 | >600 s, killed at my timeout, unfinished | - | - |
| after: `just check-cpu` cold (tests + 3 clippy + fmt, no CUDA) | 21.8 | 119 s | 453 s | 105 s |
| `just check-cuda` cold | 20.0 | 191 s | 1240 s | 85 s |
| `just e2e` (release + cuda) | 26.8 | 309 s | 1596 s | 103 s |

Honest reading: on a GPU-visible host the pin changes little (ggml already chose
the one `native` arch). The pin removes the multi-arch fallback (many more
cicc/ptxas jobs; >4x the wall of the single-arch build before I stopped it).
The real saving is the feature split: a worker's gate used to do 3 clippy variants
each building CUDA (the cargo feature sets differ), about 1000 CPU-s once per
sandbox; `check-cpu` is 453 CPU-s cold in total and no ggml-cuda is built
(verified: no nvcc/cicc in the log, no libggml-cuda in the target dir).

## sccache verdict: not enabled
Two target dirs at different paths (`sc-a`, `sc-b`), RUSTC_WRAPPER plus
CMAKE_{C,CXX,CUDA}_COMPILER_LAUNCHER=sccache, same CUDA command:
A (cold) 198 s wall / 1298 s user; B 157 s / 1043 s = same as uncached
(155 s / 1039 s). sccache stats showed CUDA (nvcc) hit rate 0.4% (1 of 254) and
Rust 0% hits; the C/C++ hits that exist are small. Absolute build paths defeat the
cache for the nvcc units. The host's sccache server is shared with other agents,
so the global counters include their traffic; the wall/CPU times are my own.
Not worth enabling; `GGML_CCACHE=OFF` kept.
Note: the harness runs a shared sccache server (SCCACHE_SERVER_UDS). I ran
`--stop-server` once and started one with a private SCCACHE_DIR, then stopped it
again; the shared store `~/.rsi/agent-sccache` was untouched and the next client
restarts the server.

## Verification
- `just check-cpu`: 822 passed / 0 failed (sum of `test result` lines), clippy x3
  clean, fmt clean, `cargo tree -p dictate-cli -e normal | rg 'whisper|dictate-fmt'`
  empty.
- `just check-cuda`: both CUDA clippy variants clean.
- `just e2e` (run once): 7 passed, WER 0.000 on short/medium/long, backend cuda.
