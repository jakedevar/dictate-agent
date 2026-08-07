---
date: 2026-08-07
researcher: claude
git_commit: b4586575d8245e5953b658575bcf6e9361378b5f
branch: master
repository: dictate_agent
topic: "R2 — Streaming partials feasibility for whisper.cpp/whisper-rs"
tags: [research, stt, whisper-cpp, whisper-rs, streaming, latency, hud]
status: complete
type: research
---

# R2 — Streaming partials feasibility for whisper.cpp/whisper-rs

## Question

Can we show live partial transcripts in the HUD while the user is still
speaking, and at what cost/quality? Locked stack: whisper-rs 0.16
(whisper.cpp bindings) + CUDA on an RTX 5080, GGUF `large-v3-turbo` primary.
Measured baseline: 183ms avg transcription via HuggingFace/PyTorch today;
whisper.cpp projected 300–500ms. Total pipeline budget: ≤1.0s from
speech-end to injected text (see master slice map, "Latency budget" table,
`thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md:133-138`).
Inject-at-end behavior is out of scope for this verdict — see Constraint
below.

## 1. What whisper.cpp's `stream` example actually does

Canonical source: `ggml-org/whisper.cpp` (renamed from `ggerganov`),
`examples/stream/stream.cpp` and `examples/stream/README.md`.
- https://github.com/ggml-org/whisper.cpp/blob/master/examples/stream/stream.cpp
- https://github.com/ggml-org/whisper.cpp/blob/master/examples/stream/README.md

The README explicitly self-labels it: *"This is a naive example of
performing real-time inference on audio from your microphone."* The
built-in VAD is called "very basic."

Defaults (`stream.cpp`, `whisper_params`): `step_ms = 3000`,
`length_ms = 10000`, `keep_ms = 200`. Two modes:

- **Sliding-window** (default, `step_ms > 0`): each tick, buffer =
  `keep_ms` of previous-window tail + `step_ms` new audio, capped near
  `length_ms`, and the **entire window is re-transcribed from scratch**
  every tick.
- **VAD-chunked** (`--step 0`): rolling 2000ms buffer, simple energy-based
  `vad_simple()` gate; once speech detected, grabs up to `length_ms` and
  transcribes the whole chunk.

Every tick calls `whisper_full(ctx, wparams, pcmf32.data(), pcmf32.size())`
— a full mel → encoder → decoder pass over the current window. The only
cross-tick continuity is **text-level**, not compute-level: prior decoded
tokens are fed back as a decoder prompt
(`wparams.prompt_tokens`/`prompt_n_tokens`), not as cached activations.

**Turbo-under-truncation risk:** no whisper.cpp GitHub issue/discussion
directly benchmarks `large-v3-turbo` quality under `stream.cpp`'s
short/truncated windows (checked issue #2439 "whisper-turbo support" and
discussion #2545 "stream.cpp improved" — neither addresses this). What is
documented is the architectural fact that large-v3-turbo prunes the decoder
from 32 layers to 4 while leaving the encoder unchanged
(https://huggingface.co/openai/whisper-large-v3-turbo). Since turbo's
capacity loss is concentrated in the decoder, and `stream.cpp`'s
chunk-and-reprompt approach stresses exactly the decoder's ability to use
short/discontinuous context, this is a reasoned architectural risk, **not**
a directly evidenced benchmark result. Flagged as the key open unknown.

General (non-stream-specific) whisper.cpp hallucination behavior is also
relevant: conditioning on previous (possibly wrong) text can "poison"
subsequent windows and produce repeat loops; standard mitigation is
disabling previous-text conditioning (`--no-context`) or pre-segmenting
with an external VAD.

## 2. whisper-rs API surface

Canonical repo (post-migration): https://codeberg.org/tazz4843/whisper-rs
— the GitHub mirror (https://github.com/tazz4843/whisper-rs) is read-only;
maintainer moved due to GitHub GenAI-features/licensing concerns for a
public-domain project. Docs: https://docs.rs/whisper-rs/latest/whisper_rs/

Confirmed API (`WhisperState`, `src/whisper_state.rs`):

```
pub fn full(&mut self, params: FullParams, data: &[f32]) -> Result<c_int, WhisperError>
```

Doc comment: *"Run the entire model: PCM → log mel spectrogram → encoder →
decoder → text ... This is usually the only function you need to call as an
end user."* This is a one-shot, whole-buffer call mirroring the C++
`whisper_full` 1:1.

- Token-level data IS exposed: `full_get_token_text[_lossy]`,
  `full_get_token_bytes`, `full_get_token_data` (→ `WhisperTokenData` with
  `t0`/`t1` timing), `full_get_token_id`, `full_get_token_prob`. Real
  token-level timestamps additionally require DTW params
  (tracked historically in whisper-rs issue #71, "Timestamps").
- A `raw-api` feature exposes the low-level `whisper_rs_sys` bindings for
  bypassing the safe wrapper if needed.
- **No state-reuse / incremental-decode API exists.** No method on
  `WhisperState` reuses encoder output across `full()` calls, no
  "decode-next-chunk-given-prior-state" call. `full()` is `&mut self` but
  stateless-per-call beyond internal scratch buffers.

**Conclusion:** whisper-rs 0.16 offers nothing beyond what raw
`whisper_full` already gives the C++ example. A streaming loop equivalent
to `stream.cpp` (rolling buffer, repeated `full()` calls, manual
prompt-token feedback via `FullParams`) would have to be **hand-rolled in
the Rust app** — we would be reimplementing the stream loop ourselves, not
consuming a supported streaming API.

## 3. GPU cost of repeated re-decode

Confirmed directly from `stream.cpp`: every tick runs full inference
(mel → encoder → decoder) over the entire current window
(`length_ms`, default 10s), refreshed every `step_ms` (default 3s). **No
encoder-state caching or tail-only decode exists in the shipped example.**
Cost per tick scales with window size (roughly constant while window is
capped), but total full-window decodes per utterance scale with
`utterance_length / step_ms` — a 10s utterance at the default 3s step
triggers ~3–4 full 10s-window decodes before the utterance even ends.

No primary GitHub source gives concrete duty-cycle/GPU-contention numbers
for this (a "streaming overhead pays off when passes are cheap" claim
surfaced only in secondary/blog-adjacent search results — not treated as
authoritative here).

**Practical implication for the RTX 5080 setup:** because each tick is a
full-window re-decode with no caching, running live partials concurrently
with the final authoritative pass means paying for N interim full-window
decodes *in addition to* the final decode, on the same GPU, inside a
workflow whose total budget is already ≤1.0s end-to-end. This is real
duty-cycle contention, not free background work — directly threatens the
S12/S21 latency budget documented in the master slice map.

## 4. Cheaper alternatives for perceived latency

**Chunk-commit / LocalAgreement** — `ufal/whisper_streaming`
(https://github.com/ufal/whisper_streaming, paper
https://arxiv.org/pdf/2307.14743): implements LocalAgreement-n (n=2) — a
prefix is confirmed/committed only once two consecutive re-decodes agree on
it; the unconfirmed suffix stays tentative and revisable. After
confirmation, the processing window scrolls forward past committed audio
instead of reprocessing it. Reported end-to-end latency ≈3.3s on
unsegmented long-form speech — well over our 1.0s total budget, using a
**single model**, not model-switching.

**Two-pass academic prior art:** "Adapting Whisper for Streaming Speech
Recognition via Two-Pass Decoding" (Interspeech 2025,
https://arxiv.org/html/2506.12154v1) — describes adapting non-streaming
Whisper via two-pass decoding; relevant prior art but not evidence of a
production-ready two-model split.

**WhisperLive** (Collabora, https://github.com/collabora/WhisperLive):
faster-whisper/TensorRT/OpenVINO backends, single model per connection by
default, `on_partial_transcript` vs `on_committed_transcript` callbacks,
Silero VAD gating to cut GPU load during silence.

**WhisperLiveKit** (https://github.com/QuentinFuxa/WhisperLiveKit): default
"SimulStreaming" policy uses attention-gated ("AlignAtt") commits for
low latency; LocalAgreement available as an alternate policy. For causal
backends they explicitly note audio is "encoded exactly once" with
"constant compute per audio second" — the clearest primary-source
articulation found of the caching-vs-full-re-decode cost distinction, and
it explicitly contrasts with `stream.cpp`-style re-decode-everything
behavior. Uses a single base model, not a small/large pair.

**Two-model (small-for-partial, turbo-for-final) approach:** no primary
source in the survey validates this as a proven production pattern — the
dominant real-world pattern across all three streaming projects is
**single model + commit/agreement policy**, not model switching. A
two-model split remains a design we would have to build and benchmark
ourselves; it is not de-risked by existing prior art.

## Verdict: DEFER

**Recommended architecture if this graduates to GO later:** single
`large-v3-turbo` instance + a hand-rolled LocalAgreement-2-style
chunk-commit loop over whisper-rs's `full()` (VAD-gated windows, prompt-
token continuity, no full-buffer re-decode past the confirmed prefix) —
matching the WhisperLiveKit/ufal pattern rather than the naive
`stream.cpp` sliding-window loop and rather than an unproven two-model
split.

**Top risk that would kill it:** GPU duty-cycle contention. Every partial
re-decode on the RTX 5080 competes with the final authoritative decode for
the same ≤1.0s speech-end→text budget that whisper.cpp turbo alone already
spends 300–500ms of (whisper-rs exposes no state-reuse/incremental-decode
API — §2 — so every partial tick is a full mel→encoder→decoder pass, §3).
Secondary risk, unconfirmed by direct benchmark but architecturally
reasoned (§1): large-v3-turbo's pruned 4-layer decoder is plausibly more
prone to flickering/unstable partials under the short, truncated,
repeatedly-reprompted windows a live-partials loop requires, which could
make the HUD affordance actively distracting rather than helpful. Revisit
once S12 lands and real turbo-CUDA latency numbers exist to size the
remaining GPU headroom — that is what turns this DEFER into a real GO/NO-GO
call.

**Constraint (unaffected by this verdict):** inject-at-end remains the
behavior regardless of GO/NO-GO/DEFER, matching Wispr Flow — partials, if
ever built, are a HUD-only affordance and are never injected as text.

## Sources

- https://github.com/ggml-org/whisper.cpp/blob/master/examples/stream/stream.cpp
- https://github.com/ggml-org/whisper.cpp/blob/master/examples/stream/README.md
- https://github.com/ggml-org/whisper.cpp/discussions/2545
- https://github.com/ggml-org/whisper.cpp/issues/2439
- https://huggingface.co/openai/whisper-large-v3-turbo
- https://codeberg.org/tazz4843/whisper-rs
- https://github.com/tazz4843/whisper-rs
- https://docs.rs/whisper-rs/latest/whisper_rs/
- https://github.com/tazz4843/whisper-rs/issues/71
- https://raw.githubusercontent.com/tazz4843/whisper-rs/master/src/whisper_state.rs
- https://github.com/ufal/whisper_streaming
- https://arxiv.org/pdf/2307.14743
- https://arxiv.org/html/2506.12154v1
- https://github.com/collabora/WhisperLive
- https://github.com/QuentinFuxa/WhisperLiveKit
- `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md:133-138` (Latency budget)
- `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md:378-380` (R2 slice definition)
