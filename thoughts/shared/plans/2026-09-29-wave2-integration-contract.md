---
date: 2026-09-29
author: Claude Opus 5.5 (RSI appointed manager ec5d83e2)
type: integration_contract
status: active
parent_plan: thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md
base_commit: 57a93f5 (Wave 1 integrated, origin/master)
---

# Wave 2+ integration contract

The master slice map stays the source of scope. This document pins what
parallel workers must agree on so their branches merge mechanically: the
post-STT pipeline order, the shared types, file ownership, and conventions.
Where a slice prompt and this document disagree, this document wins; say so in
your RESULT lines.

## 0. Ground truth (verified 2026-09-29, supersedes older notes)

- **Jake's daily driver is not the Python daemon.** It is
  `~/.local/bin/dictate-agent`, a monolithic Rust binary built 2026-06-18 from
  the pre-workspace port (whisper-rs CUDA, qwen3:14b grammar pass, Ctrl+V
  clipboard paste). i3 starts it through `scripts/run.sh`; `$mod+n` /
  `$mod+Shift+n` run `scripts/dictate-toggle` / `dictate-cancel`.
- It reads `~/.config/dictate-agent/config.toml`, which still has the
  **Python-era shape** (`[whisper] model = "openai/whisper-large-v3-turbo"`,
  `[router]`, `[editor]`, `[commands]`, `grammar.model = "qwen3:14b"`).
  `dictated` reads the same path, so config compatibility is a cutover P0.
- Its history (`~/.local/share/dictate-agent/history.db`): ~6,000 dictations
  since 2026-02-03; 99.9% plain `type` route; p50 17 words, p90 53, max ~1,500;
  end-to-end p50 0.80 s / p90 1.94 s; grammar LLM p50 0.19 s / p90 0.84 s.
  **Jake mostly dictates prompts for coding agents into ghostty** (ghostty binds
  `ctrl+v` to paste, so Ctrl+V injection works there) and Chrome.
- Observed production failure modes of the LLM grammar pass: it strips the
  leading `/` from slash commands (`/research_codebase` → `research_codebase`)
  and inserts words that change meaning ("fuzzy finding" → "a fuzzy finding").
  Protected spans and meaning-preservation guards are therefore P0, not polish.
- **The GitHub repo is PUBLIC.** Never commit real transcripts, history rows,
  window titles, or personal vocabulary. Fixtures are synthetic. Evaluations on
  real data read local paths at runtime and commit only aggregate numbers.
- `dictated`'s audio adapter keeps the microphone stream armed while idle to
  fill the 300 ms pre-roll ring (S10). That is a privacy-posture change from
  the June binary and must stay disableable (`pre_roll_ms = 0` must release the
  device while idle).
- Baseline at 57a93f5: 419 tests pass, `clippy -D warnings` clean,
  `cargo fmt --check` clean. Under RSI the exported `TMPDIR` is 83 bytes, which
  broke unix-socket test fixtures (SUN_LEN); fixed in the manager's base commit.

## 1. Post-STT pipeline order (pinned)

```
capture → VAD gate/trim → STT  (initial_prompt ← S22 vocabulary bias; language ← config/profile)
  → TEXT STAGES — deterministic, synchronous, allocation-light; ONE `fmt_rules` timing
      1. protect      S20  mark spans no later stage may alter (URLs, emails, paths,
                           slash commands, code identifiers)
      2. corrections  S20  built-in acoustic corrections (moved out of dictate-stt)
      3. dictionary   S22  sounds_like / fuzzy replacements → canonical terms protected
      4. snippets     S24  trigger phrases → protected placeholders
      5. rules        S20  spoken commands, fillers, stutters, numbers, casing, spacing
  → ROUTE   router runs on the rules output, BEFORE the LLM pass
  → LLM     S21  async, `fmt_llm` timing; only Route::Type prose; context-aware; guarded;
                 fails open to the rules output
  → RESTORE protected spans verified and restored; snippet expansions inserted
  → INJECT  S13 injector with the per-app policy resolved by S23
  → HISTORY raw STT, rules output (= grammar_input), LLM output, final text, timings,
            app context — privacy mode honored end to end
```

Invariants:

- **A stage that produces text a later stage must not alter marks it
  protected.** Protected spans are opaque to every later deterministic stage and
  must survive the LLM byte-for-byte; an LLM output that drops, duplicates, or
  edits a protected span is rejected (fail-open to the rules output).
- **Deterministic stages never change meaning by default.** Every rule that
  can delete or rewrite words defaults to the conservative choice and has
  must-not-change negative tests.
- **Routing triggers survive formatting.** Route is decided on the rules
  output, and non-`type` routes skip the LLM with `SkipReason::RouteNotEligible`.
- **Honest timings.** Each stage reports `Ran` / `Skipped{reason}` /
  `Failed{ms,error}` / `NotReported`; never a fabricated zero.

## 2. Shared types (pinned)

In `dictate-proto` (landed in the manager base commit; additive, golden-pinned):

```rust
pub enum AppCategory { Terminal, Editor, Browser, Chat, Email, Document, Other /* default */ }
pub enum Tone { Formal, Neutral /* default */, Casual, VeryCasual }
pub struct AppContext { app: String, title: Option<String>, category: AppCategory, profile: Option<String> }
```

In `dictate-fmt` (S20 creates it with exactly these fields; others may only add
fields through the integrator):

```rust
pub struct FormatContext {
    pub app: Option<dictate_proto::AppContext>, // None is the normal headless/remote case
    pub tone: dictate_proto::Tone,              // resolved from profile/category; default Neutral
    pub route: dictate_proto::Route,            // decided before the LLM pass
    pub language: Option<String>,               // detected or pinned STT language
    pub vocabulary: Vec<String>,                // in-scope dictionary phrases (S22), may be empty
}
```

S20 also owns the protected-span API (`TextDoc` or equivalent) and a
verification helper S21 calls on LLM output. S22 and S24 consume it; if they
land before S20, they expose replacements as `(byte_range, replacement)` lists
and the integrator wires them into the S20 chain.

## 3. Slice ownership (files and regions)

| Slice | Owns | Touches narrowly |
|---|---|---|
| S03 headless + E2E | `TranscribeAudio` engine/server path, `dictate transcribe`, audio-less daemon mode, config compat shim, `scripts/smoke-real.sh`, `just e2e` | `pipeline.rs` session entry (samples instead of capture), `server.rs` dispatch |
| S20 rules | `dictate-fmt` text chain + rules + `FormatContext`, `[format]` config | `pipeline.rs` formatting section, `ports.rs` `Formatter` signature, removal of corrections from `dictate-stt` |
| S22 dictionary | new `dictate-dict` crate + `dictionary.db`, dictionary proto handlers, `dictate dict …` CLI, STT prompt assembly, `[dictionary]` config | `server.rs` dictionary arms, one pipeline call site |
| S23 context | new `dictate-context` crate, profiles, `[context]` config | `pipeline.rs` session start (capture), `ResolvedOptions`, `HostInjector` policy selection |
| S21 LLM | `dictate-fmt` LLM layer + eval harness, `[format.llm]` config | replaces `GrammarFormatter` behind the S20 chain |
| S24 snippets | snippets in `dictate-dict` (`snippets` table migration), proto handlers, CLI | S20 chain stage |
| S25 command mode | EDIT route, selection capture, replace-selection injection | router, `pipeline.rs` dispatch arm |

Shared files every slice may need (`Cargo.toml` members/deps, `config.example.toml`,
`docs/protocol.md`, `CLAUDE.md`, `dictate-cli/src/main.rs` subcommand table):
append in your own block; do not reorder or reformat others' entries.

## 4. Conventions

- **Protocol:** additive only (see the crate docs). Every new wire type gets a
  golden test in `crates/dictate-proto/tests/golden.rs` and a section in
  `docs/protocol.md`. Implementing a command removes it from the server's
  `unsupported_command` list and adds a control-plane test.
- **Config:** each slice owns its section; defaults are conservative (never
  alter meaning, never add a network or device dependency by default); every
  key documented in `config/config.example.toml`; unknown keys are warned about
  once at load, never silently ignored.
- **Persistence:** only under `$XDG_DATA_HOME/dictated/` and
  `$XDG_RUNTIME_DIR/dictate-agent/`. Never write the June binary's or the Python
  daemon's paths. Schema changes are versioned migrations with upgrade tests.
- **Privacy:** privacy mode means nothing about the session is persisted —
  including window titles, dictionary hit counts tied to text, and LLM I/O.
- **Tests:** assert the intent. Synthetic fixtures only. Unit tests need no
  network, GPU, microphone, or display. Real-hardware tests (CUDA, Ollama, X11)
  sit behind a cargo feature or `#[ignore]` with a `just` target, and any X11
  test runs against a private Xvfb display — never `:0`.
- **Performance budgets (p50, RTX 5080):** text stages < 1 ms per 100 words and
  < 5 ms for 1,500 words; LLM pass ≤ 400 ms for a 50-word utterance; end to
  end ≤ 1.0 s for a 10 s utterance. Measure; do not assume.
- **Quality gates for every slice:** `just check-cpu` — `cargo test --workspace
  --all-targets` (no new failures, count ≥ base + new tests), `cargo clippy
  --workspace --all-targets -- -D warnings` plus the feature-gated targets
  (`-p dictated --features e2e-real`, `-p dictate-context --features
  x11-tests`), `cargo fmt --all -- --check`, and
  `cargo tree -p dictate-cli -e normal | rg 'whisper|dictate-fmt'` empty (the
  hotkey CLI must not link the transcription stack). The gate is CPU-only: it
  never compiles whisper.cpp's CUDA backend. The integrator runs the CUDA
  variants once per integration (`just check-cuda`, then the real-GPU
  `just e2e`, WER 0.000); a slice whose diff touches `dictate-stt`, the
  transcription path or the CUDA config runs them itself too.

## 5. Dispatch plan

- **Wave 2A (parallel, from the manager base):** S03, S20, S22, S23.
- **Wave 2B (after S20 integrates):** S21, S24, S25.
- **Wave 3:** S32 UI, S33 network API (security review before any LAN bind),
  S35 scratchpad, S42 packaging/CI, then the cutover slice (Jake's gate).
- Deferred: R2 streaming partials (NO-GO until measured headroom changes), S34
  wake word (conditional on the idle-CPU gate), S40a/S40 macOS, S41 Windows.

**Landing and review (revised 2026-10-07, current RSI standard; replaces the
per-slice review → fix → delta-review loop).** A slice lands on `master` when
the integrator's merge verification passes: compile, the touched tests, then
the full suite with no new failures, plus clippy and fmt. The operator granted
the project manager full project control on 2026-10-07, so the manager lands
and pushes `master`.

Pre-merge review happens only for two kinds of change:
- a new SQLite schema migration (for example S24's `dictionary.db` migration 3);
- credential, IAM or network-exposure changes (for example S33's LAN bind).

Either kind gets one plain reviewer pass by a model family other than the
author's, and the verdict goes in the merge commit.

Everything else lands first. An authority, custody or user-data-safety change
(clipboard handling, config writes, injection targeting) gets one post-land
review. Its findings become follow-up fix Issues, and there are no delta-review
rounds. Regressions are caught by the full suite and QA sweeps, and filed as
Issues.
