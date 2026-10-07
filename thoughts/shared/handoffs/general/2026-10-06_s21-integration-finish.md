---
date: 2026-10-06
author: Claude Opus 5.5 — worker f38a5e3d-9e28-4739-ba36-1c56fac0d957 (Issue #1409)
base: 84a9300 (integration branch; contains WIP merge d93c4c8)
code_tip: 683b0ccdb6a66dc64fce8f6df55c2b08657fc116
status: green
---

# S21 integration finish — LLM formatter verified, legacy grammar pass retired

## Commits (on top of 84a9300)

| Commit | Step | What |
|---|---|---|
| `90b103b` | 1 | `e2e_real.rs` builds `LlmFormatterPort::new(config.format.llm.clone())`; the LLM pass is off unless `E2E_LLM_MODEL` names a model |
| `1451ab4` | fix-forward | The example config carried both `[grammar]` and `[format.llm]`, so it loaded with a deprecation warning and 2 dictate-core config tests went red after the merge (the dictate-fmt example test was updated to match). `[grammar]` is now a comment about the alias; `GrammarConfig` stays as a parse target only |
| `f493731` | 2 | Removed `GrammarCorrector` (`dictate-fmt/src/grammar.rs`) and `GrammarFormatter` (core `ports.rs`). `parse_host_port` moved to `dictate_fmt::llm` (`client.rs`). dictate-fmt drops `anyhow` and `ollama-rs`. `scrub_returned_text` strips only `/no_think` and never a "thank you." (#1069, #1072) |
| `5457ac7` | 3 | doctor `grammar_model` walks the `[format.llm]` ladder: Ok if the head is installed, Warn on fallback, Fail if no rung is installed. The id is kept stable for `doctor --json` |
| `3a32d02` | docs | AGENTS.md / CLAUDE.md / FormatConfig docs describe LlmFormatter instead of GrammarCorrector |
| `b9190f2` | 4 | S23 no longer forces `llm_format = Some(false)` for terminals. They get S21's verbatim category policy; spoken punctuation and line breaks stay off. Tests, example config and `docs/protocol.md` are updated |
| `4018f18` | 5 | LOCAL history records `ExecutionResult.model` (`None` when no rung could be asked). New harness hook `Setup::with_local` and test file `tests/local_route.rs` (fake Ollama) |
| `d0f7dc0` | gate | `cargo fmt` (WIP-merge code was unformatted) |
| `683b0cc` | gate | clippy `single_range_in_vec_init` in the WIP masking test |

## Gates (at 683b0cc)

- `cargo test --workspace --all-targets --no-fail-fast`: **811 passed / 0 failed** (48 targets). The last verified commit, 6cd2be9, had 696.
- clippy `-D warnings`: workspace, `-p dictated --features e2e-real`, `-p dictate-context --features x11-tests`: all clean.
- `cargo fmt --all -- --check`: clean. `cargo tree -p dictate-cli -e normal | grep 'whisper|dictate-fmt'`: empty.
- `just e2e` (CUDA, real Whisper + Silero): **5/0, WER 0.000** on all clips. Total p50 for 3.3 / 9.3 / 33.3 s clips: 113.5 / 153.2 / 384.7 ms.

## Notes for the integrator

- **#1069/#1072 coverage.** "I just wanted to thank you." and "Thanks for the report. Thank you." survive `LlmFormatterPort::format`, `scrub_returned_text` and the LOCAL answer cleanup (`clean_answer`). The S20 `hallucination_scrub` rule still drops a trailing stand-alone "Thank you." from the *transcript*, as S20 designed (golden corpus). So the whole chain still turns "Thanks for the report. Thank you." into "Thanks for the report.". Whether to keep that is the open Jake decision already listed in the manager handoff.
- **Known flake, not S21.** In one of three full runs, `upload.rs::a_request_larger_than_the_message_limit_is_answered_then_the_connection_closes` failed with BrokenPipe. This is S03 review key `oversize_test_gate_failure`, owned by R1 (`thoughts/shared/manager/prompts/R1-review-fixes.prompt.md` item 6). It has no failure-signature record.
- **Filed #1425.** The doctor `local_model` check still ignores the `[local]` ladder and wrongly says the LOCAL route "will fail" when a fallback is installed (seen in the e2e doctor output).
- The harness's `local_model` is now `LocalConfig::default().model` instead of `"mock"`. Only the Processing notice text uses it.
- `E2E_GRAMMAR_MODEL` was undocumented and has been renamed `E2E_LLM_MODEL`.
- Next step from the manager handoff: the tier-2 review of S21 (source `0b47da8`, work key `S21`).
