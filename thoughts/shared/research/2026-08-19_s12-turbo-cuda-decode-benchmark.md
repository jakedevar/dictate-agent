---
date: 2026-08-19T15:20:00-07:00
author: Codex (RSI S12 worker)
status: complete
topic: "S12 large-v3-turbo CUDA decode benchmark and R2 decision"
tags: [benchmark, stt, whisper-rs, cuda, rtx-5080, streaming-partials]
---

# S12 — large-v3-turbo CUDA decode benchmark

## Result

**R2 streaming partials recommendation: NO-GO; retain DEFER.** The final
authoritative decode is comfortably within the S12 300–500 ms p50 budget, but
there is not evidence for safely adding a competing full decode at every HUD
tick. `whisper-rs` still exposes no incremental/state-reuse decode API. A
partial therefore costs another full mel → encoder → decoder pass. At the
observed p95 this consumes **278.351 ms** before VAD, formatting, injection,
and the authoritative final decode; overlapping two such passes produces an
unsafe p95 estimate of **556.702 ms** for STT alone and contention has not been
measured. That is not enough margin for the ≤1 s end-to-end contract, and the
existing R2 flicker risk remains. Revisit only with an incremental API or a
dedicated contention benchmark showing final-decode p95 remains within budget.

## Hardware and software

| Field | Value |
|---|---|
| GPU | NVIDIA GeForce RTX 5080, 16,303 MiB |
| Driver | 610.57.04 |
| Backend evidence | whisper.cpp logged `using CUDA0 backend`; device compute capability 12.0 |
| Provider | `dictate-stt` `Transcriber`, whisper-rs 0.16 / whisper.cpp CUDA |
| Model | `ggerganov/whisper.cpp` `ggml-large-v3-turbo.bin` |
| Pinned revision | `5359861c739e955e79d9a303bcbc70fb988958b1` |
| Model SHA-256 | `1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69` |
| Model size | 1,624,555,275 bytes |
| Decode options | greedy `best_of=1`, language pinned `en`, no initial prompt, no-speech threshold 0.6 |

## Fixture

The fixture was generated locally, not downloaded or copied into the repo:

```sh
espeak-ng -v en-us -s 145 -w fixture.wav \
  'The Dictate Agent transcribes this reproducible benchmark sentence on the NVIDIA RTX five thousand and eighty.'
ffmpeg -v error -i fixture.wav -ac 1 -ar 16000 -f f32le fixture.f32
```

| Field | Value |
|---|---|
| Source WAV | 7.95 s, mono, 22,050 Hz, 16-bit PCM |
| 16 kHz input | 127,186 f32 samples; 7,949.125 ms |
| WAV SHA-256 | `9042f90dfc56f170af6ffa40e2fe1eaf2bfbf5c5dbf61a5f702e71dba0acbd05` |
| PCM SHA-256 | `acd88fb5691508ec0a632b94abdc0781b8f04894af7788fe3cf82e2be1de3c44` |
| Recognized text | `The dictate agent transcribes this reproducible benchmark sentence on the NVIDIA RTX 5000 and AD.` |

The number transcription is intentionally reported rather than normalized; the
benchmark tests decoder latency/backend, not speech accuracy.

## Method and measurements

Command (with the required CUDA build environment):

```sh
PATH=/opt/cuda/bin:$PATH WHISPER_DONT_GENERATE_BINDINGS=1 \
  cargo run -q -p dictate-stt --bin cuda_bench -- \
  --model /home/jakedevar/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin \
  --raw-f32 /tmp/dictate-s12-bench.fcfBpr/fixture.f32 --warmup 1 --iterations 12
```

The benchmark creates one provider/model instance, excludes a single cold
warm-up from percentiles, and measures 12 subsequent complete `full()` decodes.
The initial implementation run recorded:

| Metric | Result |
|---|---:|
| Cold model load | 3,185.551 ms |
| Cold warm-up decode | 565.833 ms |
| Cold end-to-end request | 3,755.134 ms |
| Warm decode p50 | **207.300 ms** |
| Warm decode p95 | **278.351 ms** |
| Warm audio real-time factor at p50 | 0.026× |
| Warm audio real-time factor at p95 | 0.035× |

The cold model load is below S12's ≤5 s daemon-check target. The p50 is also
inside the 300–500 ms stage budget, leaving normal pipeline headroom. These are
single-authoritative-decode measurements, not evidence that an additional
concurrent partial decoder is safe.

## Recovery validation run

After reconstructing S12 into the assigned recovery sandbox, a fresh run on
the same physical RTX 5080, checksum-verified model, and regenerated fixture
completed on 2026-08-19. The fixture hashes again matched the values above;
whisper.cpp logged `using CUDA0 backend` and the provider reported `cuda`.

| Metric | Result |
|---|---:|
| Cold model load | 658.419 ms |
| Cold warm-up decode | 162.703 ms |
| Cold end-to-end request | 821.325 ms |
| Warm decode p50 | **113.262 ms** |
| Warm decode p95 | **114.314 ms** |
| Warm decodes | 12, after one excluded cold warm-up |

The lower serial timings do not reverse the decision: this measurement does
not run concurrent partial and final decoders, and whisper-rs still lacks an
incremental/state-reuse path. A GO would require a dedicated contention test
showing the authoritative final remains within budget while a competing decode
is active.
