# R1 — review fixes for S03, S22, S20 (Issue #1410)

Worker session `6ca09761-8e5a-4bf0-a28c-71bac06fffae`, branch
`rsi/6ca09761-8e5a-4bf0-a28c-71bac06fffae`, base `84a9300` (Wave 2
integration tip incl. the WIP S21 merge `d93c4c8`). Started from the saved
patch `R1-6f44c18e-uncommitted.patch` (WAV allocation/padding, pipelined
hang-up tests), which is folded into the first two commits below.

## Key → commit → regression test

Every test below was run against the old code once and failed (the S20 batch
was written first and run on the unfixed tree: 12 lib tests + 3 corpus tests +
2 perf guards failed; the S03/S22 ones by swapping the old source file back in).

| Key | Commit | Regression test(s) |
|---|---|---|
| S03 `wav_header_allocation` (+ minor `wav_odd_chunk_padding`) | `ec068d3` | `dictate-audio/tests/wav_allocation.rs::a_44_byte_header_claiming_gigabytes_allocates_almost_nothing`; `wav_container.rs::{an_odd_length_chunk_with_its_pad_byte_is_skipped_correctly, a_data_chunk_larger_than_the_file_is_malformed}` |
| S03 `abandoned_upload_injects` | `c20c162` | `dictated/tests/upload.rs::{an_uploader_that_pipelined_a_request_then_hung_up_injects_nothing, an_uploader_that_pipelined_past_the_read_ahead_bound_then_hung_up_injects_nothing}`; must-not-change `requests_pipelined_behind_an_upload_are_answered_in_order` |
| S03 `oversize_test_gate_failure` | `c20c162` | `upload.rs::a_request_larger_than_the_message_limit_is_answered_then_the_connection_closes` — 50/50 isolated runs pass |
| S03 `timeout_validation_overflow` | `09929b5` | `dictate-core config::a_timeout_too_large_for_a_duration_is_a_load_error_naming_the_key` |
| S03 `ollama_probe_panics` | `09929b5` | `ollama::a_malformed_host_is_a_clean_probe_failure_not_a_panic`, `ollama::client_accepts_http_urls_with_or_without_a_port`, `config::a_malformed_ollama_host_is_a_load_error_naming_the_key` |
| S03 `doctor_unbounded_connect` | `9838cb9` | `dictated doctor::a_socket_that_never_accepts_is_reported_within_the_deadline` |
| S22 `matcher-contraction-inside-word` | `41fde5e` | `dictate-dict/tests/dictionary.rs::contractions_and_hyphenated_compounds_are_never_split` |
| S22 `miner-joins-tokens-corrupts-targets` | `355abe5` | `tests/suggestions.rs::{a_rewrite_target_keeps_its_original_spelling, grammatical_rewrites_are_never_proposed}`; unit `grammar_is_not_vocabulary` |
| S20 `VERIFIER_REJECTS_IDENTITY_OUTPUT` | `30d05ea` | `doc::identity_output_always_verifies` |
| S20 `VERIFIER_ACCEPTS_CHANGED_URL` | `30d05ea` | `doc::a_span_continued_into_a_different_token_is_rejected` |
| S20 `NONLINEAR_PROTECTION_PATHS` | `30d05ea` | `tests/perf_guard.rs::{verifying_many_distinct_spans_stays_linear, unmatched_backtick_runs_stay_linear}` (old: 13.1 s and 2.9 s in debug) |
| S20 `LITERAL_PUA_BYPASSES_PROTECTION` | `30d05ea` | `doc::a_literal_private_use_character_does_not_unprotect_its_token` |
| S20 `SPAN_CAPACITY_CORRUPTS_TEXT` | `30d05ea`, `34bd0b3`, `c586010` | `doc::{literals_past_the_span_limit_round_trip, protection_past_the_limit_freezes_the_document}`, `corrections::a_full_document_keeps_a_command_phrase_whole` |
| S20 `CASING_CHANGES_CODE_IDENTIFIERS` | `a7b6295` | `casing::identifiers_starting_an_expression_keep_their_case` |
| S20 `NUMBER_GROUPING_BREAKS_COMMANDS` | `1a7d7c4` | `numbers::spoken_integers_are_never_comma_grouped`, `dictate-core timer::{spoken_timer_durations_survive_formatting_and_routing_exactly, a_comma_grouped_number_is_parsed_whole}` |
| S20 `CLOCK_REWRITE_BYPASSES_GLUE_GUARD` | `1a7d7c4` | `numbers::clock_times_respect_the_glue_and_digit_guards` |
| S20 `SCRUB_CORRUPTS_PROTECTED_BYTES` | `34bd0b3` | `scrub::the_scrub_never_changes_protected_bytes`, `text::{protected_bytes_survive_every_stage, protect_runs_first}`, corpus `protected_spans_survive_byte_for_byte_and_in_order` (oracle is now detection on the raw input) |
| S20 `AMBIGUOUS_ACOUSTIC_DEFAULTS` | `34bd0b3` | `corrections::ambiguous_words_and_real_dot_directories_are_kept_by_default`, corpus `the_opt_in_claude_corrections_keep_their_historical_intent` |

`UNICODE_DETECTOR_PANIC` was already fixed in `d93c4c8`.

## Decisions the integrator must know

- **AMBIGUOUS_ACOUSTIC_DEFAULTS (product decision, conservative):**
  `cloud/clod/clawed → Claude` and `.cloud/ → .claude/` are now opt-in,
  `[format.rules] claude_corrections = false` by default; the spoken slash
  commands (`create plan` → `/create_plan`, …) stay on. Meaning-preserving
  default; the narrower user-authorized route is an S22 dictionary entry
  (`Claude` sounds like `cloud`, optionally app-scoped). **Needs Jake at
  cutover:** his daily driver fixes "cloud" → "Claude" today; he should either
  set `claude_corrections = true` or add the dictionary entry.
- **Spoken integers are never comma-grouped** (`25000`, `1200000`); money keeps
  separators (`$5,000`). The timer now also parses a Whisper-written `12,300`
  whole and rejects fragments (`12,30`).
- **Chain order is now `protect → hallucination_scrub → …`** (matches contract
  §1). Artifact exceptions: `[BLANK_AUDIO]` is never protected outside
  backticks; a trailing `/no_think` is removed although protected. The scrub
  re-runs detection on what it changed.
- **Verifier semantics:** protected tokens are matched leftmost-longest and
  non-overlapping with detection's chunk rule (only openers before, only closing
  punctuation or a possessive `'s` after, up to whitespace). Consequence: an LLM
  answer that wraps a span in markdown emphasis (`**/create_plan**`) is now
  rejected (fails open to the rules output). A span glued to its neighbours by a
  stage falls back to demanding byte-identical output.
- **Raw documents:** more protected spans than placeholders → `TextDoc::is_raw()`;
  the doc is frozen (all edits refused) and only identical output verifies.
- **Casing:** a word followed by an operator (`= + - * / < > % & | ^`, `!` only
  as `!=`) keeps its case at a sentence start. Side effect: prose "well - maybe"
  is no longer capitalized.
- **Config load errors (new):** every Ollama host key (`format.llm.host`,
  `grammar.host`, `local.host`) must be an `http(s)://` URL with a host;
  `timeout_s` must convert to a `Duration`. Jake's live config sets no host keys
  and `timeout_s = 10.0`, so it still loads.
- **Upload read-ahead:** while a request is in flight the connection reads ahead
  into a carry buffer bounded by the connection's `max_message_bytes`; beyond
  the bound the socket's read-closed readiness is polled every 20 ms.
- **Doctor:** X11 socket probe is tokio's non-blocking connect under a 2 s
  deadline; the injector availability check is bounded by `PROBE_TIMEOUT`.

## Verification (final tree)

- `cargo test --workspace --all-targets --no-fail-fast`: **847 passed, 2 failed**.
  The 2 failures are pre-existing in the WIP S21 merge and reproduce on the base
  `84a9300` (checked in a detached temporary worktree):
  `dictate-core config::tests::{a_clean_current_config_produces_no_warnings,
  the_example_config_loads_without_a_single_warning_or_error}` — both are the
  `[grammar]`-deprecation warnings S21 is finishing.
- `cargo clippy --workspace --all-targets -- -D warnings`: one error, pre-existing
  in S21's `dictate-core/src/llm_formatter.rs:334` (`single_range_in_vec_init`).
  With that line locally silenced, `-p dictate-core --all-targets` is clean; all
  other targets clean. Feature-gated: `-p dictated --features e2e-real` clean,
  `-p dictate-context --features x11-tests` clean.
- `cargo fmt --all -- --check`: only S21's `llm_formatter.rs` and
  `dictated/src/lib.rs` (both pre-existing at base); every file R1 touched is
  formatted.
- `cargo tree -p dictate-cli -e normal | rg 'whisper|dictate-fmt'`: empty.

## Not done here (outside #1410's key list)

From the original brief, still open: S22 minors `dictionary-open-failure-kills-daemon`
and `recase-common-word-phrases`; the five S23 non-blocking notes
(`tail-control-chars`, `wm-class-type`, `x11-cold-connect`,
`capture-context-untested`, `live-test-timing`). Also seen, not fixed (not a
finding): the spacing rule turns `x != y` into `x!= y`.

## Conflict risk

R1 touched `dictated/src/server.rs` (S32 also edits it: connection reader and
`watch_for_hangup` only) and `dictated/src/doctor.rs` (X11/injection section
only; S21 edits the grammar section). Nothing in `dictate-fmt/src/llm/`,
`dictate-core/src/{ports,llm_formatter}.rs` or `dictate-context/src/profiles.rs`.
