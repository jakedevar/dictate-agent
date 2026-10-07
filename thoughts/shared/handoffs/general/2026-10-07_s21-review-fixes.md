# S21 post-land fixes — Issue #1441

Implementation: `4bc5db5`, on the assigned branch
`rsi/f44cbc05-2231-4470-a32e-20b3613e6801`; base `31f2ebc`.
Only dictate-fmt, the core LLM adapter, dependency metadata, tests and this
handoff changed. No push, branch changes, CUDA build, e2e run, real transcript
fixtures or changes to the live dictation daemon.

Verbatim tokens now consume dictated occurrences in order; joins, splits and
in-scope vocabulary repairs remain local. Protected placeholders anchor the
prose gaps on either side. Numeric checks retain complete signed values,
decimal/time/fraction punctuation, order and occurrence counts. Correction
explanations require a cue in the deleted run or an immediate restart; pronouns
and negations have narrower rules. Newly introduced controls, including CR,
are rejected. Conservative ambiguity falls back to the rules output.

Restore measures offsets after Unicode recasing. All segments, including
single-segment utterances, use the JoinSet panic boundary. Private session
context reaches the LLM layer, rejection logs carry validator names, and
private rejection/backend error details are redacted before health reporting.
Partial category config patches the category's own defaults; a legacy grammar
table retains its historical opt-in. One pass deadline includes model probing;
one warm-up deadline covers lookup, load and prompt-cache priming.

| Finding key | Commit(s) | Regression test(s) |
|---|---|---|
| VERBATIM_ALIGNMENT_NOT_ENFORCED | 41277f5, 25e10c0 | verbatim_requires_ordered_single_use_word_alignment; prose_cannot_duplicate_a_dictated_negation |
| UNICODE_CASE_REWRITE_INVALIDATES_SPAN_OFFSETS | d31a4d2 | continuation_recasing_keeps_unicode_span_offsets_valid; a_single_segment_panic_fails_open |
| PROTECTED_SPANS_LOSE_PROSE_POSITION | 814b9c9 | protected_spans_keep_their_relationship_to_surrounding_words |
| PRIVATE_REJECTION_LOGS_TRANSCRIPT_WORDS | a66dc6b, fea8c40, 5bedab1 | private_rejection_logs_no_dictated_words; private_context_reaches_the_llm_request |
| PARTIAL_CATEGORY_CONFIG_LOSES_VERBATIM | 26f9161 | partial_category_tables_preserve_each_categorys_defaults |
| NUMBER_VALUE_NOT_PRESERVED | 5f810fe, 25e10c0 | numeric_values_and_occurrences_survive_formatting |
| MODEL_PROBE_OUTSIDE_PASS_TIMEOUT | 7a93861 | model_probe_is_inside_the_pass_deadline |
| DROPPED_WORD_EXEMPTIONS_CHANGE_MEANING | 154da21, 25e10c0 | deletion_exemptions_cannot_remove_pronouns_or_negations |
| CARRIAGE_RETURN_BYPASSES_TERMINAL_LINE_GUARD | fa057bb | rejects_carriage_return_and_other_introduced_controls |
| WARMUP_PRIME_HAS_NO_DEADLINE | bcdecdb, f5db177 | warmup_prime_times_out_and_background_warmup_can_retry |
| LEGACY_GRAMMAR_OMITTED_ENABLED_DISABLED | 74de545, 4bc5db5 | legacy_table_without_enabled_keeps_the_historical_opt_in |

Validation:

- Touched crates: `CARGO_BUILD_JOBS=8 cargo test -j 8 -p dictate-fmt -p dictate-core`:
  **378 passed / 0 failed** (206 fmt, 172 core).
- Reviewed-base regression proof: temporarily substituted the four reviewed LLM
  modules and eval adapter from `638dc940c2f04822ed5e7ae5023edd53b4550c6b`,
  transplanting the new tests unchanged; the privacy fixture omitted the flag
  absent from that old API. Ran fmt tests and restored candidate sources in a
  Python `finally` block. **13 intended failures** covered all eleven keys:
  ten unit regressions, both HTTP deadline regressions and the log capture.
  The panic and Unicode restore checks separately failed; the additional prose
  double-negation check also failed. No fixtures or thresholds were weakened.
- Live synthetic S21 eval: `gemma4:e4b`, prompt `s21.2`, XML masks:
  **261/268 passed (97.4%, above 96% floor)**; protected spans **100%**,
  leakage **0**; four validator fallbacks (three numbers, one dropped words).
  Overall p50 **138 ms**, p95 **413 ms**; 41–70 words p50 **439 ms**.
  The daemon restart interrupted the first eval; only the completed rerun is
  reported.
- Final CPU gate: `CARGO_BUILD_JOBS=8 just check-cpu`: **873 passed / 0 failed / 0 ignored** across 44 test binaries; workspace Clippy, both feature-gated Clippy checks, formatting and CLI dependency-tree isolation passed. Exit 0.

Local evidence (not committed): `/tmp/s21-touched.log`,
`/tmp/s21-baseline-regressions.log`, `/tmp/s21-live-eval.log`,
`/tmp/s21-live-eval.json`, `/tmp/s21-check-cpu.log`.

The manager verifies at integration; this Issue requires no delta review.
RSI durable jobs cannot express the required just recipe or live example, and
the repository foreground rule conflicts with current RSI job guidance. #1466
records that limitation; the required recipes ran in the foreground with eight
Cargo jobs and load checks.

Friction: #1466

