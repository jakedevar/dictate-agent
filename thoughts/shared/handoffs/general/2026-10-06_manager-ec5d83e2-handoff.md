---
date: 2026-10-06
author: Claude Opus 5.5 — RSI appointed project manager, session ec5d83e2-c213-49ba-b211-cdedbaee507b
project: Dictate Agent (1cf08d56-8f92-47a3-8a8e-d191525117e9)
integration_branch: rsi/ec5d83e2-c213-49ba-b211-cdedbaee507b
last_verified_commit: 6cd2be996de2aebd29d647ee7169c0f18da08873
branch_tip: d93c4c8 (WIP, unverified) + this handoff commit
status: paused by operator; successor manager resumes
---

# Manager handoff — Wispr Flow parity, Wave 2

The operator paused every Claude session at 2026-09-30 07:38:47 UTC by rejecting
in-flight tool calls; the global manager has since asked for this handoff and a
fresh manager. Nothing was lost: every in-flight item is either committed or
saved as a patch committed on the integration branch (paths below).

## Operator directives (restate in every handoff)

- Act as the project manager with full autonomy ("Jesus, take the wheel"): the
  goal is a Wispr Flow copy. `~/rsi` standards are the north star.
- Make it work → make it right → make it fast. No half-assed work.
- Allowed models: Claude `claude-opus-5-5`, `claude-sonnet-5-5`; Codex
  `gpt-6.1-sol` ("SOL 6.1"), `gpt-6-luna` ("LUNA 6"). These exact IDs launch.
- Repo rules that bind every worker: `CLAUDE.md`,
  `thoughts/shared/plans/2026-09-29-wave2-integration-contract.md` (pipeline
  order, shared types, ownership, gates), and
  `thoughts/shared/manager/worker-contract.md`. **The GitHub repo is public**:
  synthetic fixtures only, never real transcripts or personal vocabulary.
- Nothing is pushed. Jake merges `rsi/ec5d83e2-…` (or its successor) into
  `master` at his gate. Do not touch his live daemon
  `~/.local/bin/dictate-agent` (June monolithic Rust build — it, not Python,
  is his daily driver), `~/.config/dictate-agent/`, or
  `~/.local/share/dictate-agent/` outside the cutover slice.

## Branch state

| Commit | What | Verified |
|---|---|---|
| `57a93f5` | origin/master: Wave 1 integrated | 419/0 |
| `d7f07f0` `ceb9ae0` `ce3484e` | manager base: socket-path test fix, proto `AppContext/AppCategory/Tone`, docs/contracts | 424/0 |
| `0375f7c` | merge S23 app context | 447/0 |
| `62637fc` | merge S22 dictionary (+ seam fixes) | 471/0 |
| `03e7b99` | merge S03 headless/doctor/E2E (+ seam fixes) | 616/0, just e2e WER 0.000 |
| `477e171` | gate feature-gated targets in every slice | docs |
| **`6cd2be9`** | **merge S20 rules + integration work — LAST FULLY VERIFIED** | **696/0, clippy ×3, fmt, CLI tree clean, just e2e WER 0.000, e2e p50 114/154/398 ms (3/9/33 s clips)** |
| `d93c4c8` | **WIP merge of S21 — UNVERIFIED** (compiled with `cargo check -p dictated --all-targets --features e2e-real`; tests/clippy/e2e not run) | no |

Integration merges carry explanatory commit messages (read `6cd2be9`'s: the
DictionaryStage, FormatContext fields, privacy log gate, etc.).

## In-flight items — exact state

### 1. S21 LLM formatter integration — WIP commit `d93c4c8` on the integration branch
Sandbox: `/home/jakedevar/.rsi/sandboxes/ec5d83e2-c213-49ba-b211-cdedbaee507b`
(this branch). Source branch `rsi/f788f9a6-418e-4dd6-a38a-49b8c8ce6de8` tip
`0b47da8`, worker RESULT 532/0 green. Worker evidence: gemma4:e4b 99.3% eval
pass (rules-only 14.9%), 100% span preservation, 0 leaks, p50 155 ms @17 words
/ 445 ms @53 words; ladder `gemma4:e4b → gemma4:12b`; `SkipReason::TooLong`
added; `[format.llm]` with the legacy `[grammar]` alias. Its handoff:
`thoughts/shared/handoffs/general/2026-09-29_s21-llm-formatting.md`.

Done in `d93c4c8`:
- `crates/dictate-core/src/llm_formatter.rs`: `LlmFormatterPort` implements
  the `Formatter` port over `dictate_fmt::llm::LlmFormatter`, feeds S03's
  `HealthTracker` (model-missing / unreachable announced once), reports the
  resolved/fallback model in status, `probe()` = ladder resolution + warm-up,
  `warm_up()` at record start. Unit tests use a fake `ChatBackend` (ready,
  missing, unreachable, validator rejection, masking, min_words override).
- Config: `FormatConfig.llm` (serde) re-derived in `dictate-core` `parse_config`
  via `LlmConfig::from_document` (applies `[grammar]` alias; warnings merged).
- `FormatContext` gains `format_llm` and `protected`
  (`TextDoc::protected_byte_ranges()`, tested); pipeline sets them; opt-out
  precedence aligned with S21 (session `Some(true)` overrides a profile's
  `llm_format = false`); `Formatter::warm_up` called after `ContextResolved`.
- `dictated::build_pipeline` now uses `LlmFormatterPort`; Ollama host from
  `config.format.llm.host`.
- Merge fixes: duplicate `dictate-proto` dep in `dictate-fmt/Cargo.toml`;
  `..FormatConfig::default()` in literals; `InjectMethod` import scoped to the
  mock module.

Remaining before S21 can be called integrated:
1. `crates/dictated/tests/e2e_real.rs` still builds `GrammarFormatter` — switch
   to `LlmFormatterPort::new(config.format.llm.clone())`.
2. Remove the dead legacy pass: `GrammarFormatter` (ports.rs) and
   `GrammarCorrector` (dictate-fmt `grammar.rs`; keep `parse_host_port`, used by
   `ollama.rs` and `local_executor.rs`). This closes Issues #1069/#1072.
3. `crates/dictated/src/doctor.rs` grammar checks (~lines 300–430) still read
   `config.grammar`; rebase them on `config.format.llm` (ladder: Ok if the head
   model is installed, Warn on fallback, Fail if none).
4. S23 default: `crates/dictate-context/src/profiles.rs` (~line 216) forces
   `llm_format = Some(false)` for the terminal category. Remove it — S21's
   category policy already makes terminals *verbatim* (fillers/false starts/
   corrections/punctuation only). Update S23's tests, the example config comment
   and `docs/protocol.md` sentence that says terminals disable LLM formatting.
5. LOCAL route history should record `result.model` (S21 note), not
   `config.local.model`.
6. Then the full gate: `cargo test --workspace --all-targets`, clippy
   (workspace, `-p dictated --features e2e-real`, `-p dictate-context
   --features x11-tests`), `cargo fmt --all -- --check`, CLI tree check, and
   `just e2e`; then request the tier-2 review of S21 (Codex `gpt-6.1-sol`
   xhigh) with source `0b47da8` on work key `S21`.

### 2. S13b — injection fidelity, stop-time destination, STT backend truth — DONE, NOT MERGED
Branch `rsi/83174acc-d75b-4341-9fef-dd69205fcbd2`, base `477e171`, RESULT
`d1eb85a` 627/0 green; Xvfb 8 cases; real E2E 7/0 incl. forced CUDA→CPU
fallback. Six commits: `aec5ed9` preserve non-text clipboards and wait for
paste consumption (#1067); `ff86fb2` resolve live destination profiles at
dictation stop (manager decision replacing #1065: destination = focus when the
user ends the dictation); `4b0d822` report the observed whisper backend
(#1068); `2c32701`, `9bde653` tests; `d1eb85a` handoff
(`thoughts/shared/handoffs/general/2026-09-30_s13b-injection-fidelity.md`).
Next: merge onto the integration branch after S21 is verified, gates, then a
review (Claude `claude-sonnet-5-5` high) and close #1065/#1067/#1068.

### 3. R1 — review-fix worker — INTERRUPTED
Session `6f44c18e-e06d-470f-816d-33794c987545`, branch
`rsi/6f44c18e-e06d-470f-816d-33794c987545`, sandbox
`/home/jakedevar/.rsi/sandboxes/6f44c18e-e06d-470f-816d-33794c987545`,
base `6cd2be9`. **0 commits.** Uncommitted work (3 modified + 4 new files) saved
as `thoughts/shared/handoffs/patches/R1-6f44c18e-uncommitted.patch`
(`git apply --binary`): `dictate-audio/src/decode.rs` (bounded WAV allocation +
RIFF odd-chunk padding), new `dictate-audio/tests/wav_allocation.rs`,
`wav_container.rs` + two WAV fixtures, and pipelined hang-up tests in
`dictated/tests/{upload.rs,harness/mod.rs}`. The matching `server.rs` fix for
`abandoned_upload_injects` was **not** on disk. Its prompt (findings with
exact keys) is saved as `thoughts/shared/manager/prompts/R1-review-fixes.prompt.md`.
Relaunch R1 from the integration tip with that prompt + the patch as a starting
point + the S20 findings below. Delta reviews afterwards: S03 `delta_of`
`4e92fb3c-4627-4f13-a721-4481ace2bc53` (keys `abandoned_upload_injects`,
`doctor_unbounded_connect`, `ollama_probe_panics`, `oversize_test_gate_failure`,
`timeout_validation_overflow`, `wav_header_allocation`); S22 `delta_of`
`45cec49d-6e38-4359-8901-9dd92c019a6f` (keys
`matcher-contraction-inside-word`, `miner-joins-tokens-corrupts-targets`).

### 4. S32 — desktop UI (Tauri) — INTERRUPTED
Session `5793b6e2-a9ce-4a7c-b718-5e27b70a895b`, branch
`rsi/5793b6e2-a9ce-4a7c-b718-5e27b70a895b`, sandbox
`/home/jakedevar/.rsi/sandboxes/5793b6e2-a9ce-4a7c-b718-5e27b70a895b`,
base `477e171`. **3 commits:** `a8ab038` get_config/set_config over the local
socket; `69ed6a4` Tauri v2 shell — protocol bridge, Flow bar logic, X11
overlay, tray; `81c356e` publish audio_level while recording. **Uncommitted:**
the whole frontend (`ui/package.json`, `bun.lock`, `index.html`, `hud.html`,
`tsconfig.json`, `vite.config.ts`, `ui/src/hub/*`, `ui/src/hud/*`,
`ui/tests/*`) saved as `thoughts/shared/handoffs/patches/S32-5793b6e2-uncommitted.patch`.
Prompt: `thoughts/shared/manager/prompts/S32-desktop-ui.prompt.md`. Relaunch
from the branch tip + patch; it still owes the i3-in-Xvfb focus/click-through
test, screenshots, and the tier-2 review (config writes, focus stealing).

### 5. S20 review — changes requested, 11 blocking findings
Assignment `06092ee0-232c-42ab-bd0d-b7feb4f83fe9`, receipt
`7e0d97e7-1046-42eb-9594-1721ce4901a0`, reviewer Codex `gpt-6.1-sol` xhigh on
source `a334a19`. Full text lives in the RSI DB:
`sqlite3 -readonly ~/.rsi/rsi.db "select finding_key, location, summary from
manager_review_findings where receipt_id='7e0d97e7-1046-42eb-9594-1721ce4901a0';"`
Blocking keys: `AMBIGUOUS_ACOUSTIC_DEFAULTS` ("The cloud is dark today." →
"The Claude…"; corrections.rs), `CASING_CHANGES_CODE_IDENTIFIERS` (`x = y` →
`X = y`; casing.rs:225), `CLOCK_REWRITE_BYPASSES_GLUE_GUARD` (numbers.rs:548),
`LITERAL_PUA_BYPASSES_PROTECTION` (protect.rs:115), `NONLINEAR_PROTECTION_PATHS`
(verify_output quadratic; doc.rs:442), `NUMBER_GROUPING_BREAKS_COMMANDS`
("timer twelve thousand…" / port numbers; numbers.rs:250), 
`SCRUB_CORRUPTS_PROTECTED_BYTES` (scrub before protect alters backtick
content; scrub.rs:30), `SPAN_CAPACITY_CORRUPTS_TEXT` (doc.rs:240/338,
lex.rs:239), `UNICODE_DETECTOR_PANIC` (protect.rs:259 — **already fixed in
`d93c4c8`**), `VERIFIER_ACCEPTS_CHANGED_URL` (doc.rs:540),
`VERIFIER_REJECTS_IDENTITY_OUTPUT` (doc.rs:448). Fold these into R1 (or a
separate S20 fix worker), then request the S20 delta review with
`delta_of = 06092ee0-…` and these keys.

### 6. Other reviews
- S23 `3e72709b-71cf-4a18-abf7-54999b1d980e`: **accepted** (0 blocking of 9).
  Its non-blocking notes are in R1's prompt.
- S22 `45cec49d-…`: changes requested, 2 blocking (in R1).
- S03 `4e92fb3c-…`: changes requested, 6 blocking (in R1).

## The two Unicode crash fixes in `d93c4c8`

1. `crates/dictate-fmt/src/text/protect.rs` `is_www`: sliced `s[..4]` on bytes;
   a token like `café:` panics (byte 4 is inside `é`). Now `s.get(..4)`.
   Regression test `multibyte_tokens_never_panic_the_detectors` (fails on the
   old code). Same bug the S20 reviewer filed as `UNICODE_DETECTOR_PANIC`.
2. `crates/dictate-dict/src/suggestions.rs` `Evidence::observe`: `ts[..10]` on
   history timestamps panics on a short/corrupt row. Now
   `ts.get(..10).unwrap_or(ts)`. (R1's miner work touches the same file —
   expect a trivial merge.)
Also added: `protected_byte_ranges_index_the_restored_text` (multi-byte offsets).

## Resume order (as proposed to the operator)

1. Finish and verify S21 on top of `d93c4c8` (items 1–6 under §1), then the
   S21 tier-2 review.
2. Merge S13b, gates, review, close #1065/#1067/#1068.
3. Relaunch R1 with its saved prompt + patch + the 11 S20 findings; then the
   S03/S22/S20 delta reviews.
4. Relaunch S32 from its branch + patch; tier-2 review.
5. Then Wave 2B remainder: S24 snippets (`Slot::Snippets`, `dictionary.db`
   migration 3), S25 command mode (EDIT route, selection capture, uses S21's
   LLM client), S35 scratchpad; S33 network API (security review before any
   LAN bind); S42 packaging/CI; and the cutover slice (Jake's gate).

## RSI bookkeeping

- Epic `2adf8e4a-059e-4288-90d4-754ea3115e73` "3 · Wave 2 — Wispr magic layer"
  under Group `2696ab8b-00b1-4bfa-9f78-bd828469a0f5`. Work rows: S03, S13b,
  S20, S21, S22, S23, S32, R1 (fence scope_version 1 / policy_version 1 at the
  time; refresh before writing). Record an integration with the `integration`
  update variant (a plain `stage` update for integration is refused).
- Created sessions: 8 of the 32-session quota (S03, S20, S21, S22, S23, S13b,
  S32, R1); DB-native reviewers do not count.
- Workers fork from `master` (the main checkout's HEAD), not from the
  integration branch: every worker prompt must start with
  `git merge --ff-only <integration tip>`. Fetch launch payloads with
  `AgentManagerPrepareControl` → `AgentManagerCommitPreparedControl`; prompts
  over 32 KiB are refused.
- Open Issues: #1065 (superseded by S13b's stop-time decision — close on
  merge), #1067/#1068 (fixed by S13b), #1069/#1072 (close when the legacy
  grammar pass is removed). Closed: #1066, #1070, #1071.
- My remaining wake (`Manager action results`, on_terminal) is daemon-owned.

## Host caveats

- Disk: RSI refuses new sandboxes below `sandbox_min_free_gib = 30`. On
  2026-09-30 the host fell to ~25 GB free and a review launch failed with
  `sandbox_capacity_refused`. Each worker sandbox costs ~7–9 GB (CUDA
  whisper.cpp build); the manager's own `target/` grows past 20 GB with repeated
  merge rebuilds — `cargo clean -p <workspace crates>` keeps the CUDA build and
  frees ~10 GB. `operator_call RunSandboxBuildCacheReclaim {dry_run:false}`
  freed 10.5 GB once. Today (2026-10-06) the host has ~196 GB free after a
  reboot, so this is not currently binding, but limit parallel builds.
- Load average was ~16 with 4–5 concurrent builds on 32 cores / 60 GB; the
  whisper.cpp CUDA compile takes ~5 min per fresh sandbox.
- `TMPDIR` under RSI is 83 bytes: unix-socket fixtures must use the
  `socket_root()` helper (already in the harnesses).

## Decisions pending for Jake (cutover)

`whisper.language = "en"` vs `auto`; mic pre-roll (300 ms keeps the input open
while idle) vs `pre_roll_ms = 0`; S21 53-word p50 445 ms vs the 400 ms budget
(decode-bound; accept or try a smaller model with `just eval-llm`);
`keep_alive` (~3 GiB VRAM vs ~2.7 s cold load); whether a whole-utterance
"Thank you." should still be scrubbed; whether uploads should force the `type`
route; one-time history import; and a human test of real pasting into ghostty
and Chrome. S03's cutover checklist is in
`thoughts/shared/handoffs/general/2026-09-29_s03-headless-e2e.md`.
