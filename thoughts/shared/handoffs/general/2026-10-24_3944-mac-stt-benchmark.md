# #3944 — on-Mac STT benchmark handoff

Recommend English Parakeet TDT 0.6B v2 via FluidAudio behind the existing
SttProvider and Rust text chain. On 30 LibriSpeech clips: WER 1.216%, p50/p95
44.7/61.7 ms, versus production-matching whisper.cpp Metal 2.888%, 860.0/943.1 ms
and isolated hub CUDA 2.888%, 116.5/140.8 ms. WhisperKit: 3.191%, 330.1/436.4 ms.
All engines have zero WER on the three existing synthetic fixtures. Parakeet
meets both #3943 adoption thresholds. Keep Whisper for multilingual fallback;
this English-only benchmark does not validate multilingual models.

Full report: `thoughts/shared/research/2026-10-24-mac-stt-benchmark.md`.
Harness and exact-byte hashes/raw measurements: `mac/bench/`. Three Swift and
Rust engines ran without Python; 396 primary + 198 verification decodes completed.
Unit tests 2/0, required `check-cpu` 1162/0, standalone CUDA/scorer clippy clean,
Mac release builds and final cache-path verification passed. No production
source, service, app installation, settings, history, tunnel or launchd changes.

Preload before readiness: Parakeet first-path Core ML specialization took 39.2 s;
subsequent new-process load was 304 ms. Model/cache hashes and process-RSS
limitations are recorded. Metal's unsupported tensor-API probe still uses Metal;
#3955 tracks better upstream M6 support. #3957 is the provider integration follow-up.
The task-specific brief and worker contract require no push; commit remains in
the shared hub object store on the assigned sandbox branch.

Ownership/conflicts: only `mac/bench/`, additive `.rsi/jobs.toml` / `justfile`
recipes, and these thoughts artifacts. No production crate or `mac/README.md`
edits. Base `5b36baf`. Author: Codex gpt-6.1-sol high, session
86a762aa-ba1b-4663-9e45-ff7695da7413.

Friction: RSI #3954, #3955
