# English Mac STT helper (#3957)

`dictate-parakeet` runs FluidAudio revision
`c79c4106b954f81205113c3343e5a7e59a1089b5`, the exact runtime benchmarked in
#3944. Parakeet TDT 0.6B v2 is English only. Raw results enter dictated's existing
Rust VAD/text chain/router; this package does no formatting, capture or paste.

The Mac installer builds the helper with SwiftPM, installs it beside dictated in
`~/Library/Application Support/dictated/bin/`, downloads the precompiled models
to `~/Library/Application Support/dictated/models/parakeet-tdt-0.6b-v2/`, verifies
all 22 files against the benchmark hashes, then preloads. The hashes are compiled
into `ModelManifest.swift`; a mutable sidecar cannot approve different bytes.
Every helper startup verifies again and loads locally, without network access.
A missing/corrupt model fails closed. Upstream changes cannot silently replace
the benchmarked model. No Python or model conversion is involved.

From the hub checkout, with SSH configured:

```sh
export DICTATE_MAC_BUILD_ID=pm-3957
just mac-dictated-setup
just mac-dictated-build
just mac-dictated-test
just mac-dictated-install
```

Each build runs nice 10, at most six jobs, inside that ID's `.noindex` directory.
For RSI use the assigned session ID, not the example PM ID. Install preserves an
existing config. New configs explicitly select Parakeet. To migrate the original
#3945 config (which has no `[stt]` section), run on the Mac after installation:

```sh
data="$HOME/Library/Application Support/dictated"
# The guard refuses duplicate tables; if [stt] already exists, edit its provider.
if ! grep -Eq '^\[stt\][[:space:]]*$' "$data/config.toml"; then
  printf '\n[stt]\nprovider = "parakeet"\nlanguage = "en"\n' >> "$data/config.toml"
fi
"$data/bin/dictated" --config "$data/config.toml" --check-config
~/dictate-mac-build/pm-3957.noindex/rust/mac/scripts/dictated-launchagent.sh restart
```

The effective settings are:

```toml
[stt]
provider = "parakeet"
language = "en"
# Optional overrides (defaults shown):
helper_path = "~/Library/Application Support/dictated/bin/dictate-parakeet"
model_dir = "~/Library/Application Support/dictated/models/parakeet-tdt-0.6b-v2"
```

English is explicit; other configured/per-request languages are refused. Whisper
settings apply only when `provider = "whisper"`. To use the configurable
multilingual fallback, change that one provider key, configure `[whisper]`
`model = "large-v3-turbo"`, `device = "metal"`, `language = "auto"` (or a specific
language), and restart the same LaunchAgent. The installer retains/pulls the
pinned Whisper model too. Recovery never switches engines silently. Parakeet
does not consume Whisper's initial_prompt/vocabulary bias; deterministic Rust
dictionary/snippet stages still apply. Client/hub endpoint fallback belongs to
DictateHub (#3961); this helper does not change endpoints or credentials.

The API is available during preload so `/v1/status` shows `loaded=false`;
dictated logs readiness only after verified Core ML loading succeeds. Initial
preload has a 120 s bound (first Core ML specialization was about 40 s). It stays
resident until daemon shutdown. Request processing is serialized with one
queued request. A dropped/cancelled caller does not kill a healthy helper: the
worker drains its response and discards it, then handles the next request.
Queued cancelled work is skipped. A timeout/crash/protocol error kills the
helper, clears loaded status, and the next request makes one restart attempt
with exponential backoff (250 ms base, capped at 5 s). There is no audio replay
or infinite retry loop. Per-request processing, including restart, is bounded
by `3 * audio_seconds + 5` seconds; initial daemon preload is separate. A queued
request can also wait for the previous bounded operation. Shutdown aborts the
worker and kills the owned child. Model status reports `backend=coreml` only
after verified loading. Third-party transcript diagnostics are disabled.

## Wire protocol

Private stdin/stdout pipes only, no listener. Every request is:

1. u32 little-endian number of bytes that follow (excludes this prefix).
2. u32 little-endian UTF-8 JSON header size, at most 4096 bytes.
3. Header `{"id":1,"sample_rate":16000}`.
4. Mono f32 little-endian PCM, at most 9,600,000 samples (ten minutes).

Nonfinite samples, partial frames and other sample rates are refused. The helper
prints exactly `{"ready":true}\n` after preload, then one JSON line per request:
`{"id":1,"text":"...","ms":45.0}` or `{"id":1,"error":"decode_failed"}`.
Responses are bounded to 64 KiB. IDs must match. stderr carries only diagnostics;
no text is logged. Rust does not include third-party errors in request logs.

## Verification and live regression

`just mac-parakeet-test` runs Swift framing tests and builds release. Linux's
`just check-cpu` also executes the Rust shell-helper tests (framing, residency,
crash/timeout restart, cancellation, invalid/oversized input, malformed/oversized
responses and failed preload). `just mac-dictated-test` runs them on Darwin plus
the UDS/daemon tests and the stub LaunchAgent test.

Recreate #3944's 33 public/synthetic fixtures with `mac/bench/` from its benchmark
commit, or copy its already-generated `work/samples/` into this session's
`~/dictate-mac-build/<id>.noindex/rust/build/parakeet-fixtures/`. Verify against
`mac/bench/results/samples.sha256` before using them. Never commit audio.
With the LaunchAgent running Parakeet, `just mac-parakeet-bench` warms every clip
once then measures three passes through authenticated `POST /v1/transcribe`.
The token goes to curl's stdin only. JSONL results include per-clip WER edits,
pass totals and nearest-rank p50/p95; normalization matches `devtool wer`
(lowercase ASCII words, preserving apostrophes). `ms` is curl's HTTP total,
including server decode/VAD, helper IPC, Rust text chain and response, excluding
curl process startup. These end-to-end numbers differ from #3944's raw STT.
Only these public fixture results belong in git.

`just mac-parakeet-helper-bench` runs the same fixtures through a separate
instance of the installed helper using the production binary protocol. It
records `results/helper.jsonl`, excluding VAD and Rust formatting, to separate
provider accuracy from pipeline changes. Its latency includes helper IPC and
response parsing; it excludes file I/O and frame construction. It has no API
credential, service, settings or injection side effect. The service's own helper
stays resident. A helper regression starts with one warm-up per clip and then
three measured passes, like the API regression.

The LibriSpeech fixtures are public audiobook excerpts, distributed under
CC BY 4.0 by OpenSLR SLR12 (Panayotov, Chen, Povey and Khudanpur, ICASSP 2015).
No operator recordings, history or vocabulary are regression inputs.
