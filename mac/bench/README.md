# Mac STT benchmark (#3944)

Python-free, file-only STT harnesses. No microphone, app installation, paste,
service, tunnel, settings, or launchd operations. The Swift package is independent
of DictateHub; the two Rust packages are independent of the production workspace.
The Rust decoder pins the same whisper-rs 0.16.0 / whisper-rs-sys 0.15.0 code as
`dictate-stt`, with the same greedy / English / no-context / no-speech defaults.
It sets six CPU threads and otherwise retains whisper.cpp defaults, including
flash attention off. The compiled Metal backend is verified in the runtime log.

From the repository root, use the declared RSI recipes (long commands belong in
`AgentSubmitJob`, omit `timeout_minutes`):

1. `mac-stt-bench-swift`: prepare samples, build and run FluidAudio + WhisperKit.
2. `mac-stt-bench-whisper`: verify/copy the pinned hub model, build and run Metal.
3. `mac-stt-bench-hub`: check >=4 GiB free VRAM, run the isolated CUDA baseline
   once, score all engines, run scorer tests and both Rust clippy checks.
4. `mac-stt-bench-verify`: verify the final FluidAudio cache path and Rust source,
   run both again into `results/validation/`, and hash the model files.

Each target is also a `just` target with that name. `run.sh` uses `ssh mac-mini`
(or `DICTATE_MAC_HOST`) and `~/dictate-mac-build/<id>.noindex/bench/`, where id is
`DICTATE_MAC_BUILD_ID`, `RSI_SESSION_ID`, or the assigned `rsi/<id>` branch suffix.
It refuses an unsafe id. It does not use `mac/scripts/remote.sh`'s `--delete`
sync, because downloaded benchmark inputs must survive each source sync.
All Mac builds use `nice -n 10` and at most six build jobs. Rust tools and targets
live in this benchmark's `work/`; an existing Homebrew cmake is read, otherwise
a prebuilt CMake distribution is unpacked inside `work/`. No brew install,
sudo, Python, pip, conda, model conversion, or live daemon is needed.

Models live below `~/Library/Application Support/dictated/models/` (0700):

- `ggml-large-v3-turbo.bin` (catalog SHA checked before use).
- `parakeet-tdt-0.6b-v2/` (FluidAudio v2; precompiled Core ML).
- `models/argmaxinc/whisperkit-coreml/openai_whisper-large-v3-v20240930_turbo/`
  plus `tokenizers/` (WhisperKit; precompiled Core ML).

SwiftPM revisions and transitive dependencies are pinned in `Package.resolved`;
Rust dependencies are pinned in each `Cargo.lock`. Model download APIs resolve
upstream model repositories, so verify `results/models.sha256` against a rerun
rather than assuming a model name alone pins bytes. Keep this manifest when
models change. FluidAudio's `to:` parameter resolves a sibling repository;
the harness supplies the full repository directory to keep it under `models/`.

`Prepare` downloads no data itself: the shell fetches OpenSLR's LibriSpeech
`test-clean.tar.gz`; Swift selects 30 clips, one per speaker in lexical speaker
order, taking the first lexical utterance lasting 3–15 s, and invokes `afconvert`
for PCM16 mono 16 kHz WAV. The three existing synthetic e2e clips are appended.
`results/manifest.tsv`, `samples.sha256`, and `corpus.sha256` identify the inputs.
Reference texts and WAVs are regenerated in ignored `work/samples/`.

Every engine preloads all audio, loads its model once, warms every clip once,
then emits three passes of 33 JSONL records. Timings wrap raw STT only; audio file
read/resampling, VAD, text rules, LLM formatting, HTTP and injection are excluded.
WhisperKit uses English, temperature zero, no fallback sampling, no timestamps,
and one concurrent worker. Parakeet uses FluidAudio's default compute placement
and a fresh decoder state per clip. `prepare` after the arguments fetches/loads
without decoding, to separate download/setup from cold-process startup.

Score a completed run without rerunning inference:

```sh
cargo run --locked --manifest-path mac/bench/score/Cargo.toml -- \
  mac/bench/work/samples/manifest.tsv \
  mac/bench/results/FluidBench.jsonl mac/bench/results/KitBench.jsonl \
  mac/bench/results/whisper-metal.jsonl mac/bench/results/whisper-cuda.jsonl
```

The Rust scorer reuses `devtool wer`'s normalization/edit-distance algorithm:
lowercase ASCII words, retain apostrophes inside words, other punctuation becomes
a separator. It sums word errors/reference words, reports each pass separately,
and uses nearest-rank p50/p95 over every warm clip measurement in each set.
It rejects missing/duplicate clips and incomplete passes. WER is not rounded
before aggregation. Core ML/Metal memory outside the process is not included in
RSS; treat RSS as process memory, not total unified-memory use.

Only source, model/input hashes, public/synthetic output transcripts, timing logs,
and summaries belong in git. Never commit downloaded audio, models, private
recordings, config, tokens, or `work/`.

LibriSpeech attribution: Vassil Panayotov, Guoguo Chen, Daniel Povey, and Sanjeev
Khudanpur, *LibriSpeech: an ASR corpus based on public domain audio books*, ICASSP
2015. The corpus is distributed under CC BY 4.0 by
[OpenSLR SLR12](https://www.openslr.org/12/). These are public read audiobook
samples, not operator recordings.
