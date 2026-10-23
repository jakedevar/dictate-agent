# Mac-local STT benchmark — #3944 / plan #3943

Recommendation: **Parakeet TDT 0.6B v2 through FluidAudio for English Mac
dictation**, behind the existing `SttProvider` and the single Rust text chain.
Retain whisper.cpp Metal for multilingual/configurable fallback and the hub as
fallback. This benchmark changes no production provider, service, or app.
Integration is filed as **#3957**; #3945 can continue its transport/LaunchAgent
work without treating the benchmark harness as a production service.

The requested report filename says 2026-10-24; measurements actually ran on
2026-10-23 UTC. Base: `5b36baf`. Author session:
`86a762aa-ba1b-4663-9e45-ff7695da7413`, Codex `gpt-6.1-sol`, effort high.

## Results and decision

Warm raw STT, milliseconds. Each row has one warm-up per clip, followed by three
measured passes. WER counts are per pass; all three passes have the same error
counts. Latencies are pooled per set, nearest-rank quantiles. Source JSONL and
per-clip error counts are committed in `mac/bench/results/`.

| Engine / host | Set | Clips / warm measurements | Errors / words | WER | p50 ms | p95 ms |
|---|---|---:|---:|---:|---:|---:|
| whisper.cpp Metal / M6 | LibriSpeech | 30 / 90 | 19 / 658 | 2.888% | 860.0 | 943.1 |
| WhisperKit turbo / M6 | LibriSpeech | 30 / 90 | 21 / 658 | 3.191% | 330.1 | 436.4 |
| **Parakeet v2 / M6** | **LibriSpeech** | **30 / 90** | **8 / 658** | **1.216%** | **44.7** | **61.7** |
| whisper.cpp CUDA / RTX 5080 | LibriSpeech | 30 / 90 | 19 / 658 | 2.888% | 116.5 | 140.8 |
| whisper.cpp Metal / M6 | synthetic e2e | 3 / 9 | 0 / 127 | 0.000% | 867.3 | 1914.1 |
| WhisperKit turbo / M6 | synthetic e2e | 3 / 9 | 0 / 127 | 0.000% | 353.6 | 982.1 |
| **Parakeet v2 / M6** | **synthetic e2e** | **3 / 9** | **0 / 127** | **0.000%** | **46.2** | **84.6** |
| whisper.cpp CUDA / RTX 5080 | synthetic e2e | 3 / 9 | 0 / 127 | 0.000% | 123.2 | 307.8 |

#3943 adopts an alternative when WER is at least 1 percentage point better
absolute **or** p50 is at most half Metal. Parakeet meets **both**: 1.672 points
better LibriSpeech WER and 0.052× Metal p50 (19.3× faster). It is also better than
the current hub raw-STT baseline on this corpus: 1.672 points lower WER and 2.61×
faster p50. WhisperKit meets the speed threshold (0.384× Metal), but Parakeet is
both faster and more accurate, so it is the recommended alternative.

This recommendation chooses English-first behavior. Parakeet v2 was the
explicitly permitted English candidate; this supplies no evidence for v3's
multilingual accuracy. A multilingual default remains a distinct product choice;
keep the existing Whisper option until that choice is benchmarked.

Per synthetic clip, warm p50 (three runs) is useful because the pooled p50 above
mixes very different clip lengths:

| Clip / duration | Metal | WhisperKit | Parakeet v2 | Hub CUDA | Recorded hub STT p50 / p95 |
|---|---:|---:|---:|---:|---:|
| short / 3.3 s | 781.0 | 240.9 | 40.3 | 97.0 | 105.4 / 108.4 |
| medium / 9.3 s | 867.3 | 353.6 | 46.2 | 123.2 | 135.0 / 135.6 |
| long / 33.3 s | 1911.0 | 972.7 | 84.3 | 305.0 | 332.8 / 337.2 |

The historical hub numbers are from
`thoughts/shared/research/2026-09-29-s03-e2e-latency.md`, the identical committed
synthetic WAVs, eight measured runs after warm-up, WER zero. Those measure the
daemon STT stage after VAD, so they are contextual evidence, not interchangeable
with this raw-STT harness. The fresh CUDA run supplies a directly comparable
baseline for **all 33 clips**, including LibriSpeech, absent from that history.

## Startup and memory

| Engine | Model initialization in a new process, files present | Maximum process RSS |
|---|---|---:|
| whisper.cpp Metal | 5.730 s initial; 5.566 s verification | 1835.4 MiB |
| WhisperKit turbo | 5.432 s after separate download/preparation | 264.8 MiB |
| Parakeet v2 | 39.230 s on first load at the final cache path; 0.304 s on the next process launch (0.163 s at original warmed path) | 520.2 MiB including first-path Core ML specialization; 124.3 MiB original warmed run |
| hub CUDA | 0.380 s | 569.9 MiB |

“Cold” here means a new process with models already downloaded; filesystem caches
were not dropped. Core ML specialization caches are managed by macOS. Moving the
Parakeet cache caused a one-time 39 s load despite identical model bytes; the
following new process loaded in 304 ms. Service integration must preload before
advertising readiness and keep the provider resident. Initial download/setup
runs took 59.8 s for FluidAudio and 844.6 s for WhisperKit, including network
transfer and initialization; those are not pure model-load measurements.

RSS is `getrusage(RUSAGE_SELF).ru_maxrss` / macOS `/usr/bin/time -l`, converted to
MiB (2^20 bytes). It excludes separately accounted ANE/GPU/driver memory and is
**not total unified memory**. Core ML's low process RSS does not imply its model
weights consume no memory. No swaps were reported by the Mac process timers.

## Inputs, method, and environment

- Mac: Apple M6, `Mac18,5`, 16 GiB RAM, macOS 27.0.1 (26A434), arm64,
  Swift 6.4, Rust 1.99.1, CMake 4.4.4. `nice -n 10`, at most six build jobs.
  Another worker was building the macOS daemon on this Mac; no idle/exclusive
  hardware claim. All three candidates built and ran Python-free.
- Hub: RTX 5080, 16,303 MiB VRAM, driver 615.71.09. Immediately before the
  isolated baseline 7,909 MiB was free; the harness rechecked >=4,096 MiB before
  allocation. It used its own process, six inference CPU threads, and at most
  eight build jobs. No daemon was started, signalled, stopped, or reconfigured.
- Three synthetic fixtures are the repository's short/medium/long e2e WAVs and
  references. No operator recordings were accessed: none are available outside
  private history, and that history was not read.
- [LibriSpeech test-clean](https://www.openslr.org/12/): 30 clips from 30 speakers,
  deterministic lexical speaker order, first eligible lexical utterance each;
  3.03–13.48 s, 245.08 s total, 658 normalized reference words. Converted on the
  Mac with `afconvert` to mono 16 kHz PCM16 WAV. Corpus attribution: Vassil
  Panayotov, Guoguo Chen, Daniel Povey, Sanjeev Khudanpur, ICASSP 2015; distributed
  under CC BY 4.0, derived from public-domain LibriVox audiobooks.
- Every input is loaded before timing. Each engine loads once, warms every clip
  once, then processes three passes in identical order. Timing wraps model
  inference and transcript assembly only, including async completion. No audio
  file I/O/resampling, VAD, text chain, LLM formatting, IPC, or injection is timed.
- WER uses the existing `devtool wer` algorithm copied into the Rust scorer:
  lowercase ASCII, punctuation becomes word separators, apostrophes retained
  within words; word-level Levenshtein errors divided by reference words. Corpus
  WER is micro-averaged, not a rounded average of per-clip percentages. Repeated
  passes are not independent accuracy samples (only 658 distinct natural words).
- Greedy Whisper English, no initial prompt/context, no-speech threshold 0.6;
  whisper-rs 0.16.0 / whisper-rs-sys 0.15.0, matching production. Flash attention
  remains off, as in the current production default. WhisperKit uses English,
  temperature zero, no fallback sampling, no timestamps, one concurrent worker.
  FluidAudio uses v2 with default compute placement and independent decoder state
  for every clip. There is no extra text cleanup in any candidate.

## Artifacts and exact models

Harness: `mac/bench/README.md`, `Package.swift`, `Sources/`, standalone Rust
`whisper/` decoder and `score/` WER tool, thin SSH scripts. Revisions are pinned
in SwiftPM/Cargo lockfiles. Results: `mac/bench/results/summary.jsonl`, each
engine's 99 JSONL measurements, runtime logs, model/sample SHA-256 manifests,
input selection manifest, and separate final-source validation runs. No audio or
model files are committed.

Model root: `~/Library/Application Support/dictated/models/`, verified mode 0700.
`mac/bench/results/models.sha256` contains **all 50 runtime model/tokenizer files**
(excluding download metadata). Paths below are relative to that root:

| Model/runtime | Exact artifact and identity |
|---|---|
| whisper.cpp | `ggml-large-v3-turbo.bin`, 1,624,555,275 B; SHA-256 `1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69`, verified against `dictate-stt` catalog revision `5359861c739e955e79d9a303bcbc70fb988958b1` |
| WhisperKit | `models/argmaxinc/whisperkit-coreml/openai_whisper-large-v3-v20240930_turbo/` + `tokenizers/`; runtime revision `9f99b5cb6a907730543823ad271b82532782a899`; every component hash in `models.sha256` |
| Parakeet | `parakeet-tdt-0.6b-v2/`, FluidAudio revision `c79c4106b954f81205113c3343e5a7e59a1089b5`; Encoder weight SHA `4adc7ad44f9d05e1bffeb2b06d3bb02861a5c7602dff63a6b494aed3bf8a6c3e`, Decoder `27d26890221d82322c1092fd99d7b40578e435d5cf4b83c887c42603caf97aba`, JointDecision `ca22a65903a05e64137677da608077578a8606090a598abf4875fa6199aaa19d`, Preprocessor `a5f7df6c7f47147ae9486fe18cc7792f9a44d093ec3c6a11e91ef2dc363c48dc`; vocabulary and all component files also hashed |

Model APIs resolve their upstream repositories at download time. Committed hashes
pin the measured bytes, while source lockfiles pin runtimes. No conversion,
Python, pip, or conda was used. The archive SHA-256 is
`39fde525e59672dc6d1551919b1478f724438a95aa55f874b576be21967e6c23`.

Official runtime references read for the implementation:
[WhisperKit](https://github.com/argmaxinc/WhisperKit/tree/9f99b5cb6a907730543823ad271b82532782a899),
[FluidAudio](https://github.com/FluidInference/FluidAudio/tree/c79c4106b954f81205113c3343e5a7e59a1089b5).

## Validation, engineering choices, and limits

- Primary comparison: **396/396 measured decodes**, zero execution failures,
  plus warm-ups. Final-source verification: **198/198 decodes**, WER unchanged;
  Parakeet LibriSpeech p50/p95 40.0/60.5 ms, Metal 835.3/914.9 ms. Primary results
  are retained separately, rather than selecting the fastest repeat.
- Scorer unit tests **2 passed / 0 failed**, clippy clean for both standalone
  Rust packages including the CUDA decoder. `just check-cpu` **1162 passed /
  0 failed across 62 test binaries**, all three clippy lines, fmt and CLI dependency
  isolation passed. Shell syntax and standalone Rust formatting passed.
- Durable jobs: primary Swift `99534549-7e8c-4205-80b0-516a66b87a81`, primary Metal
  `989d69df-6963-442f-86da-168edd03a4db`, isolated hub + Rust checks
  `6a6b6ca2-9ced-45cd-a855-bb8136b49e4a`, final Mac validation
  `8fbfa35a-4cbb-4db4-b054-267227736719`, required CPU gate
  `d8f588e1-6cee-4dfa-b3e4-38fe7bfdce2e`. All passed.
- The original FluidAudio `to:` argument caused its sibling-directory resolution
  to place this task's download beside `models/`. Corrected to the full repository
  directory, moved only that task-created directory under `models/`, and verified
  the final source there. This also exposed the cold Core ML specialization cost.
- Metal logged a failed tensor-API feature probe, then explicitly selected the
  Metal backend with 1623.92 MB model allocation. It did **not** fall back to CPU.
  Follow-up **#3955** tracks upstream M6 support; optional Core ML encoder,
  quantized Whisper, flash attention and newer upstream revisions were not tested.
  The recommendation compares the specified production-matching default.
- This is a small, clean, read-English corpus and robotic fixtures. It does not
  prove accuracy on noisy microphones, personal vocabulary, accents outside the
  selected speakers, conversational disfluencies, or multilingual dictation.
  It does establish the plan's measured adoption rule and same-sample raw-STT
  superiority over the hub. Final hotkey/API/paste acceptance belongs to #3957
  and the remaining #3943 integration slices.
- A missing durable-job session-id environment variable required deriving the
  isolated build id from the assigned branch (RSI **#3954**). No shared `manual`
  build directory was used. The hub lacks `/usr/bin/time`; the Rust decoder now
  reads peak RSS through `getrusage`. Early harness compile errors were corrected;
  the Swift wrapper now finishes both candidates and returns failure if either
  fails. No unrelated production files were changed.
- Work stayed in the assigned sandbox and the isolated Mac build directory, plus
  the shared model cache after the corrected relocation above. `/Applications`,
  DictateHub settings/TCC, tunnel, launchd, hub service/config/history were untouched.
  Per the task-specific brief and worker contract, commit only; do not push.

Friction: RSI #3954, #3955
