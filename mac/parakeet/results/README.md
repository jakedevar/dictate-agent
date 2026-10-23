# #3957 live regression (2026-10-23 UTC)

Author: Codex gpt-6.1-sol, high, RSI session
`e90aaba0-3e00-43b6-ae9d-84ab4088dd27`. Host: mac-mini, Apple M6, macOS 27.0.1.
Production Rust/helper implementation was built from source `8704ab7`; the later
commits add benchmark tooling, fixture hashes and evidence only. Installed binary
SHA-256 values are in `binaries.sha256`. FluidAudio is pinned to
`c79c4106b954f81205113c3343e5a7e59a1089b5`; the 22 model hashes in
`Sources/DictateParakeet/ModelManifest.swift` match #3944's inventory.

`manifest.tsv` and `samples.sha256` are byte-for-byte from benchmark commit
`89f76e34feead531e4b103f0057857201e0447c0`. All 66 WAV/reference hashes passed
(`input-check.txt`). No audio/model files or operator recordings were committed.
These are 30 public LibriSpeech test-clean utterances (658 reference words) and
three synthetic repository fixtures (127 words). One warm-up per clip, three
measured passes, 99 measured decodes for each path; every decode succeeded.

| Path | LibriSpeech WER | p50 / p95 ms | Synthetic WER | p50 / p95 ms |
|---|---:|---:|---:|---:|
| Installed helper, private framed PCM pipes | 1.215805% (8/658) | 36.577 / 47.736 | 0% (0/127) | 39.856 / 93.799 |
| Installed dictated, authenticated HTTP UDS | 1.823708% (12/658) | 196.984 / 323.444 | 0% (0/127) | 252.027 / 676.567 |
| #3944 Parakeet raw benchmark | 1.215805% (8/658) | 44.7 / 61.7 | 0% | 46.2 / 84.6 |
| #3944 whisper.cpp Metal raw benchmark | 2.887538% (19/658) | 860.0 / 943.1 | 0% | 867.3 / 1914.1 |
| #3944 hub CUDA raw benchmark | 2.887538% (19/658) | 116.5 / 140.8 | 0% | 123.2 / 307.8 |

WER repeats identically in all three passes. Metrics use lowercase ASCII words,
punctuation as separators and apostrophes preserved, matching `devtool wer`.
Percentiles use nearest rank across all warm measurements in a set. Raw helper
latency includes PCM-pipe I/O/response parsing and excludes file I/O/frame
construction. UDS latency is curl's HTTP total: upload decoding/resampling,
VAD, helper work, the Rust text chain and response delivery; it excludes curl
process startup. Pacing between HTTP requests respects the unchanged S33 rate
limit and is outside measured time. The LaunchAgent and diagnostic process have
different launch/scheduling contexts, so their timing difference cannot all be
attributed to transport overhead. Raw benchmark latency is not an end-to-end
hub/API comparison; no claim of hub end-to-end parity is made from these rows.

The installed helper retains the exact raw WER from #3944. The API adds one edit
on each of four clips:

- `1188-133604-0000`: `Tintoret` -> `Tintorette`.
- `2094-142345-0001`: leading `but` -> `For`.
- `5683-32865-0003`: adds trailing `but`.
- `5105-28233-0000`: `fourteen` -> `14` (intentional number formatting).

The first three are acoustic differences consistent with VAD changing the input;
that cause has not been isolated with an A/B run. #3971 tracks the comparison.
Keep the existing Rust pipeline behavior rather than changing VAD defaults from
this inference. The number edit is an expected metric effect, not lost meaning.
The API's literal WER is still 1.064 percentage points better than the benchmarked
Metal/CUDA raw WER on this small clean English subset. It establishes neither
microphone/noisy accuracy nor multilingual performance.

`second-client.txt` records an authenticated synthetic short-WAV POST: HTTP 200,
expected transcript, 186.473 ms HTTP total, 148.466 ms STT, 34.613 ms VAD. Injection
is skipped as `not_permitted`. `whisper-fallback.txt` / `whisper-status.json`
record the explicit config switch to Whisper, successful identical transcript
(HTTP 200, first observed request 3458.184 ms), loaded backend `metal`. This was
a fallback smoke, not a warm benchmark. The config was restored and the real
LaunchAgent restarted: `restored-parakeet.txt` records HTTP 200 / expected text,
276.289 ms first observed POST after restart. `status.json` is the final idle,
loaded `parakeet-tdt-0.6b-v2`, `coreml` status. The credentials were unchanged.

Live admission checks returned 401 without bearer, 403 for a hostile Origin and
421 for a hostile Host. `runtime.txt` shows one resident helper, TCP listeners
zero, run directory 0700, socket/token/config 0600. Private capabilities remain
false; no microphone, injection, app, TCC or hub service was touched. `tcp.txt`
is deliberately empty (lsof found no listeners). App hotkey/local-first/hub
fallback acceptance belongs to parallel #3961 and was not tested here.

Primary UDS job: `51b12622-5d54-4dad-bc0c-2d8986a51874`.
Installed-helper verification: `027366b6-1c8d-4bd8-95df-b77b68dc2879`.
