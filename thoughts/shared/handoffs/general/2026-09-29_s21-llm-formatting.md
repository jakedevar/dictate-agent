---
date: 2026-09-29
author: Claude Opus 5.5 (RSI worker)
slice: S21 — formatting LLM layer + eval harness
base: ce3484e804d53f5c0ee371ead0994b8c2f3a3f12
branch: rsi/f788f9a6-418e-4dd6-a38a-49b8c8ce6de8
status: green
research: thoughts/shared/research/2026-09-29-s21-llm-eval.md
---

# S21 handoff — LLM formatting layer + eval harness

## Result

`dictate_fmt::llm`: a guarded, fail-open Ollama formatter with its own eval
harness. On the 268-case synthetic corpus, `gemma4:e4b` passes **99.3 %**
(rules-only baseline 14.9 %), keeps **100 %** of protected spans, **0**
leaks; p50 **155 ms at 17 words**, **445 ms at 53 words** (11 % over the
400 ms budget — decode-bound, see "Needs Jake"). Real-history tier (500 real
dictations, aggregates only, strictest mode): 4.9 % fail open on validators,
0 infrastructure errors. Workspace: **532 passed / 0 failed** (baseline
424), clippy `-D warnings` clean (also with `--features eval-history`),
`fmt --check` clean, `dictate-cli` tree has no whisper/dictate-fmt.

## What is where

| Path | What |
|---|---|
| `crates/dictate-fmt/src/llm/mod.rs` | `LlmRequest`, `LlmGate`, `LlmPlan`, `LlmOutcome`, `LlmFormatter` (`plan`, `format`, `format_traced`, `refresh`, `start`, `warm_up`, `warm_up_in_background`, `health`, `unload`) |
| `llm/client.rs` | thin `/api/chat` + `/api/tags` client behind `ChatBackend`; error classes Unreachable / ModelMissing / Timeout / Http / Malformed |
| `llm/resolve.rs` | `ModelResolver` ladder + `LlmHealth` (`Disabled`/`Unchecked`/`Ready{model,missing_preferred}`/`Unavailable{reason,installed_alternatives}`), loud-once logging, 30 s back-off |
| `llm/protect.rs` | span normalization, fallback technical-token detector, leading-span detach, `<kN/>` masking, verify/restore |
| `llm/validate.rs` | 14 validators (see research §4) |
| `llm/prompt.rs` | prompt `s21.2`: verbatim / prose / email × structure × tone, few-shot, delimiters |
| `llm/chunk.rs` | sentence/paragraph chunking that tiles the input exactly |
| `llm/config.rs` | `[format.llm]` (`LlmConfig`), `LlmConfig::from_document` (legacy `[grammar]` alias, unknown-key warnings, clamping) |
| `llm/eval.rs` | corpus/lint/scoring/report, `ReplayBackend`, `RecordingBackend` |
| `tests/llm_fail_open.rs` | fake Ollama over real HTTP: down, missing model (+ re-resolution), none installed, timeout, malformed, 500, every validator, skip rules, masking, chunk fallback, warm-up |
| `tests/llm_eval_recorded.rs` + `tests/eval/` | CI tier: corpus (268), mask stress (48), recordings (316 replies), floor 96 % |
| `examples/eval_llm.rs` | live tier (`--features eval-live`; `just eval-llm`), latency/warm-up/mask sweeps, `--record`; history tier (`--features eval-history`) |
| `dictate-core/src/local_executor.rs` | LOCAL route on the same ladder/health (E) |

## Integration wiring (for the integrator, ≤ 30 lines)

```rust
// dictated::build_pipeline — config: parse the file once more as a table (or
// embed `llm: LlmConfig` in S20's FormatConfig and still call this for the
// [grammar] alias and unknown-key warnings):
let llm = Arc::new(LlmFormatter::new(LlmConfig::from_document(&raw_table)?.config));
// dictated::run, after ensure_ollama_running: resolve ladder + warm up, off-path
tokio::spawn({ let llm = llm.clone(); async move { llm.start().await } });
// status/doctor: serde of llm.health() and pipeline.local.health() (additive proto field)
// session start (→ Recording, after S23 resolves context):
llm.warm_up_in_background(ctx.category.clone(), tone.clone());
// pipeline formatting region, AFTER S20 rules and AFTER routing (contract §1):
let req = LlmRequest { text: doc.text().to_owned(), protected: doc.protected_byte_ranges(),
    category: fc.app.as_ref().map(|a| a.category.clone()).unwrap_or_default(),
    tone: fc.tone.clone(), vocabulary: fc.vocabulary.clone(), language: fc.language.clone() };
let gate = LlmGate { route: fc.route.clone(), profile_llm_format: profile_llm_format,
    session_format_llm: opts.format_llm };          // SessionOptions.format_llm
let text = match llm.plan(&req, &gate) {
    LlmPlan::Skip(r) => { stages.timings.fmt_llm = StageTiming::skipped(r); req.text.clone() }
    LlmPlan::Run => { let clock = StageClock::start();
        let o = /* self.race(&token, llm.format(&req)) → Cancelled: clock.failed(..), finish */;
        stages.timings.fmt_llm = match &o.error { Some(e) => clock.failed(e.clone()), None => clock.ran() };
        interaction.grammar_input = Some(req.text.clone()); interaction.grammar_output = Some(o.text.clone());
        interaction.grammar_changed = o.changed; interaction.grammar_duration_s = Some(o.duration.as_secs_f64());
        interaction.grammar_error = o.error.clone()
            .or_else(|| o.validator_rejection.as_ref().map(|r| format!("partial: {r}")));
        o.text } };                                  // spans already restored and verified
// LOCAL route: history/notice should use `result.model`, not `config.local.model`.
```

`o.error.is_some()` ⇔ the text is the input unchanged (→ `Failed{ms}`); a
chunked input can apply some chunks (`segments_applied < segments`, `error`
None, `validator_rejection` set) → `Ran`. Privacy mode: `LlmOutcome` holds
text; persist it only where the pipeline already persists grammar fields.

## Decisions (and why)

1. **Thin HTTP client instead of `ollama-rs`**: needed the HTTP status and
   Ollama's error body (model-missing vs down), `done_reason` (truncation),
   and a load-only request; reqwest was already in the graph.
2. **Mask `<k1/>`** by evaluation (0 span rejections over 316 cases, best
   stress pass); leading spans are **detached** so the model never sees them.
3. **Ladder `gemma4:e4b` → `gemma4:12b`**: e4b is better and 2.4× faster;
   12b is a guarded fallback. qwen3.6 models were not run (no VRAM without
   eviction).
4. **Terminal/editor = verbatim**: deletions and punctuation/case only, no new
   words, no new line breaks (a newline can execute in a shell). Tone only
   applies to prose.
5. **Meaning guards beyond the brief**: `dropped_words` (deleting a clause or
   negation is a meaning change) and a code-symbol check (`x += 1` from
   "x plus equals one" is execution) — both found by running 12b.
   `span_correction`: a self-correction right after a span cannot be resolved
   safely; keep it verbatim or fall back.
6. **Chunking**: one request ≤ 90 words; above, sentence-boundary chunks run
   sequentially (parallel slots measured no gain); skip > 300 words with the
   new additive `SkipReason::TooLong` (golden-pinned).
7. **Skip rules**: `Some(false)` session → `Disabled`; profile/category off →
   `Disabled` unless session `Some(true)`; `Some(true)` also overrides
   `min_words` but not the global switch, route, length cap or availability;
   words inside protected spans do not count; known-unavailable →
   `DependencyUnavailable` without a per-utterance connection attempt.
8. **`enabled = false` in code** (contract §4: no network dependency by
   default); the example config turns it on. Legacy `[grammar]` maps
   enabled/host/min_words, `timeout_s` → `timeout.max_ms`, and `model` goes
   *first* on the default ladder — Jake's `qwen3:14b` then resolves to
   `gemma4:e4b` with one loud "preferred not installed" warning instead of
   failing every dictation. `[format.llm]` present ⇒ `[grammar]` ignored, warned.
9. **Fallback protect detector** (`protect_fallback = true`) unions with
   S20's spans — defence in depth for a P0 failure class.
10. **Warm-up primes the prompt cache** (first request after a cold load 242
    → 167 ms p50); `warm_up_in_background` dedupes and never blocks.

## Files outside `dictate-fmt/src/llm/` (conflict risk)

`crates/dictate-fmt/Cargo.toml` (deps: dictate-proto, reqwest, serde_json,
regex, toml, optional rusqlite; features; example), `dictate-fmt/src/lib.rs`
(`pub mod llm;`), `dictate-proto/src/timings.rs` + `tests/golden.rs`
(`SkipReason::TooLong`), `docs/protocol.md` (one sentence),
`config/config.example.toml` (`[grammar]` deprecation note, new
`[format.llm]` block, `[local].models`), `justfile` (appended `eval-llm`),
`dictate-core/src/config.rs` (`LocalConfig.models`),
`dictate-core/src/local_executor.rs`. Not touched: `grammar.rs`,
`pipeline.rs`, `ports.rs`, S20 chain files.

## Needs Jake

- **53-word latency is 445 ms p50 vs the 400 ms budget** (decode-bound on
  e4b; nothing client-side helps). Accept, or pull a smaller model and run
  `just eval-llm <model>` — the harness decides.
- **`keep_alive`** trades ~3 GiB VRAM for formatting the first short
  utterance after an idle period (a cold load takes ~2.7 s and falls back).
  Default `30m`.

## Follow-up issues filed

- #1072 — legacy grammar pass (`grammar.rs`) strips a dictated trailing
  "Thank you." via `scrub_returned_text`; closes when `LlmFormatter` replaces
  `GrammarFormatter` (S21's path never calls it).

## Housekeeping

Models I loaded were unloaded at the end (`keep_alive: 0`); a private
Ollama used for the parallelism measurement (port 11555, separate HOME,
`OLLAMA_NOPRUNE=1`) was stopped. Jake's `dictate-agent`, config and data
were only read (history DB opened read-only; aggregates only).
