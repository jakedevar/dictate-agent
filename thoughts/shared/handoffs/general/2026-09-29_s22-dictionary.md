# S22 dictionary handoff

Implemented on the assigned RSI branch from ce3484e804d53f5c0ee371ead0994b8c2f3a3f12.
Checkpoint: 1e2e567 (store, matcher, miner). The final RESULT identifies the tip
containing daemon/CLI wiring and this note. No live daemon/config/history was touched.

## Delivered

- `dictate-dict` owns SQLite CRUD and immutable Aho-Corasick matcher snapshots;
  no dependency on core, formatting, Whisper, or hardware. Default DB is
  `$XDG_DATA_HOME/dictated/dictionary.db`, with WAL and transactional ordered
  `PRAGMA user_version` migrations. V1 creates entries; V2 adds JSON app scopes.
  S24 should append migration 3 in `store.rs` for snippets. Future versions fail
  explicitly, and empty/V1 upgrades and persistent reopen/WAL are tested.
- Entries retain every wire field, Unicode caseless unique phrase keys, JSON
  aliases/scopes, and creation/edit timestamps. CRUD validates limits and ids,
  reports conflict/not-found/invalid-params separately, and ignores client hit counts.
- Exact case-insensitive aliases (or explicitly case-sensitive variants), Unicode
  word boundaries, longest non-overlapping replacements, possessives/punctuation,
  recasing, app scopes, and ambiguity refusal. Unicode folding uses current Rust
  lowercase pairs plus full case folding, with original UTF-8 boundary mapping
  so an expanding character cannot be partially matched. Explicit case-sensitive
  aliases may include several case variants. No normalization of spelling.
- Fuzzy matching is optional single-token edit similarity (minimum six characters,
  at most two edits, unique best candidate, default threshold 0.9). It ships OFF.
  The 330-sentence must-not-change corpus validates DEFAULT exact matching;
  that evidence is not sufficient to enable fuzzy matching by default.
- Per-session bounded natural glossary prompt, manual first then hit count/edit
  recency/id; static Whisper prompt takes priority, whole terms fit the remaining
  budget. Hard limit: 400 Unicode characters AND UTF-8 bytes, even if configured
  higher. Enabled global/in-scope vocabulary is exposed for S21.
- Hit counts track actual replacements (including recasing), not unchanged terms.
  They queue in memory and flush on a blocking worker every two seconds and at
  shutdown; failed database batches remain queued and emit diagnostics. Both
  session and global history privacy suppress increments. Counts are aggregate,
  never associated with retained transcript text.
- Real server CRUD, proposals, capability denial, additive apps/suggestion wire
  types/goldens, config and protocol docs. Suggestions require dictionary_read
  AND history_read; writes require dictionary_write.
- CLI list/add/rm/enable/disable/suggest/accept/import/export. JSONL import maps ids
  by canonical phrase and ignores imported hit counts; export never truncates an
  omitted-limit list. JSON parsing completes before any import mutation. A record
  failure after earlier successful records leaves those records installed and is
  reported (documented; batch transactions are not promised by the wire protocol).
- Auto-learn issues only SELECTs against history: a stable contiguous token/phrase
  rewrite of at most four tokens, >=3 observations on >=2 UTC days, or capitalized
  non-initial/CamelCase/acronym terms seen >=5 times. Conflicting rewrite targets,
  insertions/deletions, failed grammar passes, punctuation-only edits, known terms,
  and sentence-initial capitals do not produce rewrite proposals. Proposals carry
  counts/days/first/last seen and never persist themselves. Explicit acceptance is
  the existing upsert operation, with auto_learned provenance. No transcript logging
  was added to the miner.

## Integration instructions / shared-file risks

- `Pipeline.dictionary: Option<Arc<Dictionary>>` is built in `dictated::build_pipeline`
  and shared with ServerDeps. Mock constructors use None or an isolated in-memory
  store; absent stores withdraw dictionary capabilities honestly.
- `ResolvedOptions.use_dictionary` resolves SessionOptions, defaults true, and
  gates prompt assembly and replacement together. S21 must use the same gate when
  calling `dictionary.vocabulary(app)` for FormatContext; do not enable vocabulary
  when this flag is false. No S21 LLM prompt implementation is in this slice.
- Pipeline changes are narrow bindings around STT, plus required option/dependency
  fields and terminal metadata. Raw STT text stays raw in history/wire results;
  the repaired text is handed onward. `PipelineOutcome.dictionary` retains the
  Applied stage output and replacement output byte ranges, explicitly BEFORE
  later formatting (these are not final-transcript offsets).
- As required by the integration contract, MOVE the dictionary application block
  into S20 chain stage 3, after protect/corrections; use Applied.replacements to
  protect canonical replacement spans. The standalone base has no S20 protected
  API, so protection/offset movement through subsequent stages belongs to that
  integration. S20's single fmt_rules clock must include this work. Existing
  NotReported rules timing was left honest rather than inventing a zero.
- S23 should replace the temporary AppContext::new(opts.app) bridge with its
  captured/resolved AppContext (scope needs only app). Global entries apply when
  no context exists; app-scoped ones do not.
- Expected merge joins: workspace deps/lock, config aggregation/example, proto
  command/result goldens, CLI main/render, dictated lib/server, pipeline fields
  and STT bindings, engine option resolution. Existing unsupported-dictionary
  assertions were updated alongside new positive CRUD/negative capability tests.
  `dictate-hotkey` was touched only to supply the new optional Pipeline field in
  its test constructor. No S20 formatting implementation or S23 capture logic
  was changed.

## Verification (observed finish, exact numbers)

- Baseline: `cargo test --workspace --all-targets`: **424 passed / 0 failed**;
  first CUDA build finished in 6m05s. Foreground execution followed the direct
  worker contract, which overrides generic catalog advice about durable jobs.
- Final workspace: **448 passed / 0 failed**, 25 result groups, no ignored tests.
  Log: `/tmp/S22-test.log`. Net +24 tests (including 330 negative corpus sentences
  and additional table scenarios inside those tests).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean, final observed
  finish. `/tmp/S22-clippy.log`.
- `cargo fmt --all -- --check` and `git diff --check`: clean.
- `cargo tree -p dictate-cli -e normal | rg 'whisper|dictate-fmt|dictate-dict'`:
  no matches. The production CLI speaks protocol only.
- Real daemon control plane + built CLI: **19 commands passed** via
  `python3 scripts/test-dictionary-cli.py`, including CRUD, disable/enable,
  portable JSONL export/import/update, suggestions/acceptance, and malformed
  import leaving the dictionary unchanged. `/tmp/S22-cli-roundtrip.log`.
  The committed dictionary_daemon example uses only mock hardware, synthetic
  history, an explicit /tmp root, and no PID-file claim or live display access.
- Release matcher: `cargo run --release -p dictate-dict --example latency`,
  **500 entries, 100 words, 10,000 samples, two replacements per utterance**:
  **p50 0.016 ms / p95 0.022 ms / p99 0.036 ms** on this host. Final log:
  `/tmp/S22-latency.log`. Benchmark is pure matching, excluding prompt assembly,
  recording/STT, snapshot compilation and asynchronous SQLite hit flushing.

## Follow-up

Filed RSI Issue **#1066**, id `f814073e-19f8-5596-9181-a1ba0df23d03`:
pre-existing pipeline tracing emits raw/grammar transcript text even in private
sessions. This is outside S22's owned logging/formatting region and was not
changed. Acceptance calls for synthetic tracing-capture tests and transcript
redaction while preserving operational diagnostics. Dictionary hit-count privacy
is implemented and tested independently of this inherited logging defect.
