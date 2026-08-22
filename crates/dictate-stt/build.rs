// Build notes for the whisper-rs CMake build:
//
// Production CUDA builds need `/opt/cuda/bin` on PATH and
// WHISPER_DONT_GENERATE_BINDINGS=1. Those durable environment settings live in
// the workspace `.cargo/config.toml`; `just` exports the remaining PATH piece.
// CPU CI uses `--no-default-features --features cpu-tiny-ci` and does not need
// CUDA hardware or the large production model.
fn main() {}
