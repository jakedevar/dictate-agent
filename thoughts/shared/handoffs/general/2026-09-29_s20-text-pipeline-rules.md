---
date: 2026-09-30
slice: S20
work_key: S20
author: Claude Opus 5.5 (RSI worker 074c9be7)
base: ce3484e804d53f5c0ee371ead0994b8c2f3a3f12
status: green
---

# S20 — text pipeline foundation + deterministic formatting rules

## What landed

- **`dictate_fmt::text`**, the chain every later text stage plugs into:
  - `TextDoc` holds working text plus protected spans.
  - `TextStage` is the synchronous stage trait.
  - `TextChain` has `Slot::Dictionary` and `Slot::Snippets` for S22/S24.
  - `FormatContext` has exactly the five fields pinned in contract §2.
  - The detectors are `detect_spans` / `Protect`.
  - `TextDoc::verify_output` checks LLM output.
  - Stages with their own matchers use the `Edit`/`Replacement::Protected` API, which is also how a stage protects what it produces.
- **Rules**, each a named stage with a `[format.rules]` toggle:
  - on by default: `hallucination_scrub`, `builtin_corrections`, `fillers`, `stutters`, `numbers`, `spacing`, `casing`, `terminal_punctuation`;
  - off by default: `spoken_punctuation`, `spoken_line_breaks`.
- **Pipeline**, formatting section only:
  - The chain runs right after STT and reports `fmt_rules` as `Ran{ms}`, or `Skipped{Disabled}` when `[format] enabled = false`.
  - The router runs on the rules output. Non-`type` routes skip the LLM with `RouteNotEligible`.
  - `Formatter::plan`/`format` take `&FormatContext`.
  - LLM output that drops, duplicates, reorders or edits a protected span is rejected: `fmt_llm` becomes `Failed{"formatter output rejected: protected slash_command `/research_codebase` is missing or altered…"}` and the session keeps the rules output.
  - History: `grammar_input` = rules output (always set), `grammar_output` = LLM output (including a rejected one), `corrected_transcription` = final text, and `raw_transcription` = raw STT.
- `dictate-stt` returns raw Whisper text and no longer depends on `dictate-fmt`.
- Docs: `[format]` is documented in `config/config.example.toml`, with a test that keeps the example equal to the defaults. AGENTS.md and CLAUDE.md are corrected for the new order.

## Decisions the integrator and S21/S22/S24 must know

1. **Scrub runs before `protect`** (a deviation from contract §1, which lists protect first). `/no_think` is a trailing artifact shaped exactly like a slash command, so protecting first would make it unremovable. The scrub only deletes known trailing artifacts and normalizes whitespace, so it cannot corrupt a protected span. Final order: scrub → protect → corrections → [dictionary] → [snippets] → spoken punctuation/line breaks → fillers → stutters → numbers → spacing → casing → terminal punctuation.
2. **Placeholders.** Each span becomes one Supplementary PUA-A character (`U+F0000+i`).
   - The rules' token editor refuses to delete or rewrite a placeholder (`debug_assert!` plus a release no-op), so "no rule alters a span" holds by construction.
   - `apply_edits` rejects any batch touching a placeholder or containing a forged one.
   - Private-use characters already in the input are escaped as `Literal` spans.
3. **The one sanctioned span edit.** `.clod`/`.cloud`/`.clawed` → `.claude` inside detected `Path` spans (`~/.cloud/settings.json`) goes through a **crate-private** `amend_span`. External stages cannot amend spans.
4. **The LLM sees restored plain text, not placeholders.** Verification is whole-token and count-plus-order based. `/research_codebases`, `x/research_codebase` and `~/.claude/x` (from `~/.claude`) all fail it; a trailing sentence period after a span passes. Where the guard lives:
   - **In the pipeline**, so every `Formatter` (including S21's) is guarded.
   - When `[format] enabled = false`, `TextDoc::protected()` still detects spans for this guard, because it is a safety property, not a formatting preference.
5. **`FormatContext.route` is provisional inside the chain.** It holds the forced route, else `Type`, because routing happens on the chain's output. The pipeline sets the resolved route before `plan`/`format`. `app` is `AppContext::new(opts.app)` until S23.
6. **Conservative choices worth reviewing:**
   - A trailing "Thank you." is scrubbed only when it is its own sentence. "I wanted to thank you." is kept; the old substring scrub deleted it.
   - A bare "Thank you." utterance is still scrubbed to empty (historical behavior; see *Needs Jake*).
   - `", um,"` keeps one comma ("I think, we should"). It could be a list separator; S21 can smooth it.
   - Numbers:
     - commas only from 10,000 (`port 8000` stays pasteable), but money groups from 1,000 (`$5,000`);
     - ordinals and "twenty second" stay words;
     - "one second" / "one minute" stay idioms;
     - a run of number words that doesn't parse as exactly one number is left alone ("twenty twenty four", "one two three").
   - A lowercase `i` becomes `I` only with pronoun evidence (sentence start, a preceding conjunction, or a following pronoun-verb/-ed word), never in variable context. Whisper already capitalizes the real pronoun, so in code prompts a lowercase `i` is usually a loop variable.
   - No space is ever inserted after `.` or `:` (`config.yaml`, `std::fs`, `12:30`).
   - Camel-case proper nouns (`GitHub`, `iPhone`) are protected as code. The effect is that an LLM changing their casing is rejected (fail-open).
7. **Corrections now emit proper-noun `Claude`** for every word form (historical ` cloud` → `claude` was lowercase), match whole words case-insensitively, and never fire inside URLs or code (`cloud.google.com`, `cloudy`, `cloud-native`, `iCloud` are safe). All 23 historical intents have a test each.

## Measured numbers (release, RTX 5080 host, `cargo run --release -p dictate-fmt --example chain_bench`)

| input | p50 | p90 | p99 | budget |
|---|---|---|---|---|
| 17 words (Jake's p50) | 5.4 µs | 5.6 µs | 8.9 µs | — |
| 100 words | 29.1 µs | 29.4 µs | 50.9 µs | < 1 ms |
| 1,500 words | 436 µs | 668 µs | 778 µs | < 5 ms |

- **Heaviest stage:** `protect` (~145 µs per 1,500 words).
- **Regression guard** (`tests/perf_guard.rs`): 15,000 words best-of-3 must finish under 2 s. It takes ~50 ms in debug, so it can't flake. The guard also covers pathological inputs — 15,000 consecutive fillers, stutters, backtick spans, and a 60 KB chunk of `)` — that previously went quadratic. For example, 15,000 × "um" took 4.3 s before the editor got O(1) alive-token links.

## Verification

- `cargo test --workspace --all-targets`: **496 passed, 0 failed** (baseline 424; −1 removed STT corrections test; +73 new).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean. `cargo fmt --all -- --check`: clean. `cargo tree -p dictate-cli -e normal | rg 'whisper|dictate-fmt'`: empty.
- Golden corpus: `crates/dictate-fmt/tests/golden_corpus.rs` has 176 invented pairs (coding prompts, chat, email, notes, already-clean text, routing triggers). The idempotence and span-preservation properties hold over all of them.
- Session tests: `crates/dictated/tests/formatting.rs` runs 8 real-daemon socket tests. A mutation check confirmed the slash-strip test fails without the guard, reproducing the production `research_codebase` output exactly.
- Real hardware: espeak-ng synthetic speech → CUDA large-v3-turbo via `cuda_bench` (no microphone, no daemon, model read-only) → chain.
  - Whisper's `Um, so research code base for how the retry logic…` became `So /research_codebase for how the retry logic…`.
  - Already-clean Whisper output was unchanged.

## Conflict risks (files outside `dictate-fmt` I touched)

- `crates/dictate-core/src/pipeline.rs`:
  - formatting section, with the route block moved above the LLM;
  - `Pipeline.text_chain` field;
  - `Stages::new` comment;
  - `pub use dictate_fmt::TextChain`;
  - two tests.
- `crates/dictate-core/src/ports.rs`: `Formatter` signature, `MockFormatter` (`rewriting`, `calls`, `last_context`), `pub use FormatContext`.
- `crates/dictate-core/src/config.rs`: `format` field plus two tests.
- `crates/dictated/src/lib.rs`: builds the chain from `config.format`.
- `crates/dictated/tests/harness/mod.rs`: `Setup.text_chain` and `with_text_chain`.
- `crates/dictated/tests/control_plane.rs`: `"hello there"` → `"Hello there"` in 11 assertions; `fmt_rules` NotReported → Ran.
- `crates/dictate-hotkey/src/lib.rs`: test pipeline literal.
- `crates/dictate-stt` (corrections removed).
- `config/config.example.toml`: new `[format]` block plus the `[grammar]` comment line.
- AGENTS.md, CLAUDE.md.
- **S22/S23/S03** will also add `Pipeline` fields and harness `Setup` fields next to mine. These conflicts are adjacent-line conflicts only.

## Follow-up issues filed

- #1069: the LLM-response scrub in `grammar.rs` deletes a legitimate trailing "thank you." (S21).
- #1070: privacy mode still logs `Transcribed: …` at info level (outside S20's section).
- #1071: `config.example.toml` puts whisper `language`/`initial_prompt` under `[vad]` (S03).

## Needs Jake

A whole utterance of "Thank you." is still scrubbed to nothing: it is Whisper's classic silence hallucination, and the June binary did the same. Now that VAD gates silence, a VAD-positive "Thank you." is more likely real speech. Say if you want it kept.
