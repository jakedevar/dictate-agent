---
date: 2026-08-07T12:00:00-05:00
researcher: Claude (RSI master orchestration session)
git_commit: 26dbf21cce8a4607adc7edf1d603d2ba584a3d74
branch: rsi/bb349467
repository: dictate_agent
topic: "Wispr Flow parity — master orchestration slice map (Rust rebuild)"
tags: [plan, orchestration, slice-map, rust, wispr-flow, whisper, tauri, api-server, parity]
status: active
last_updated: 2026-08-07
last_updated_by: Claude (RSI Epic-lead session bba123fa)
last_updated_note: "Wave 0 dispatched (S00 + R1–R4). Ground truth corrected: 78-test baseline verified, src_rust_archive discrepancy resolved, Python daemon deletion by 5e16667 found and folded into S00 Task 1."
type: master_slice_map
---

# Wispr Flow Parity — Master Orchestration Slice Map

## Mission

Rebuild `dictate-agent` as a Rust-first dictation platform with functional
parity to [Wispr Flow](https://wisprflow.ai/) (backend behavior, not UI
polish), running on Arch Linux first and macOS second (Windows optional),
with a stretch goal of a network API server so other machines on the LAN can
submit dictation to this PC. UI, where needed, is a Tauri v2 desktop app
(TypeScript allowed); the daemon must be fully functional headless.

Our structural advantages over Wispr Flow: **fully local/private** (no cloud),
**Linux support** (Wispr has none), **self-hostable API**, and Jake's existing
routes (systemd timers, local LLM executor) that Wispr doesn't offer.

## Ground truth (assets already in hand)

| Asset | State | Where |
|---|---|---|
| Python daemon (reference impl) | **DELETED on local `master` by commit `5e16667`**; intact on `origin/master`; S00 restores it | `dictate/` (2,304 LOC, 13 modules) |
| **Rust port, Phases 1–7** | Code-complete vs Python daemon; builds; **78 tests pass (verified 2026-08-07, exit 0)**; clippy/deploy artifacts outstanding | `Cargo.toml` + `src/` (2,783 LOC, 13 modules) |
| Rewrite feasibility research + real benchmarks | Final: Rust + whisper-rs decision locked | `thoughts/shared/research/2026-04-10-rust-go-rewrite-feasibility.md` |
| 7-phase rewrite plan | Executed | `thoughts/shared/plans/2026-04-10-rust-rewrite.md` |
| Implementation handoff (open items list) | Phases done, cleanup tasks listed | `thoughts/shared/handoffs/general/2026-04-10_17-51-32_rust-rewrite-implementation.md` |
| History analytics plan | Prior art for S30 | `thoughts/shared/plans/2026-02-03-interaction-history-analytics.md` |
| Real perf data (3,595 interactions) | avg transcription 0.183s (HF/5080), avg total pipeline 0.87s; whisper.cpp projected 0.4–0.6s | feasibility doc §Whisper Performance |
| whisper-rs build traps | Documented: `WHISPER_DONT_GENERATE_BINDINGS=1`, `PATH="/opt/cuda/bin:$PATH"`, API deltas from 0.14→0.16 | handoff §Learnings |

**Repo-state discrepancy — RESOLVED by direct inspection 2026-08-07 (Epic-lead):**
`src_rust_archive/` does not exist. The Rust port is live at `src/` and is the
single source of truth. The stale `CLAUDE.md` that claimed otherwise was itself
deleted by `5e16667`, so the contradiction is moot; S00 writes a fresh one.

**Destructive-commit finding (Epic-lead, 2026-08-07) — drives S00 Task 1:**
Commit `5e16667` ("saving agent created work") deleted the entire Python
reference daemon `dictate/` (13 modules, 2,304 LOC) and `CLAUDE.md` alongside
legitimate Rust improvements. This contradicts locked decision #8 (side-by-side
migration) and the v1.0 cutover gate, both of which require the Python daemon
to stay runnable. Nothing is lost: `dictate/` is intact on `origin/master`, and
local `master` is 5 commits ahead / 0 behind, all unpushed. S00 restores
`dictate/` from `origin/master` as its first, standalone commit.
Note `pyproject.toml` still declares `packages = ["dictate"]` and the
`dictate.main:main` entry point, so the tree is currently inconsistent with its
own packaging manifest until that restore lands.

## Parity target — Wispr Flow feature matrix

Verified against wisprflow.ai and 2026 third-party reviews (sources in
§References). Mapping each Wispr capability to the slice(s) that deliver it:

| # | Wispr Flow feature | Priority | Slice(s) |
|---|---|---|---|
| 1 | Dictate into any app at cursor, no plugins | P0 | S02, S13 |
| 2 | Push-to-talk (hold) + toggle + hands-free auto-stop | P0 | S31, S11 |
| 3 | Auto punctuation/caps/formatting (lists, paragraphs, emails) | P0 | S20, S21 |
| 4 | Filler-word removal ("um", stutters, restarts) | P0 | S20, S21 |
| 5 | Mid-sentence self-correction ("…at 5, actually 6pm" → "at 6pm") | P0 | S21 |
| 6 | Tone/style matching per app (Formal / Casual / Very casual) | P1 | S23, S21 |
| 7 | Personal dictionary; auto-learn names/jargon | P0 | S22 |
| 8 | Snippets (spoken shortcut → expansion) | P1 | S24 |
| 9 | Command mode (voice-edit selected text: "make this formal") | P1 | S25 |
| 10 | 100+ languages, auto language detection | P1 | S12 (config + auto-detect) |
| 11 | Whisper mode (very quiet speech) | P1 | S10 (AGC/gain), S12 |
| 12 | Low perceived latency (~1s speech-end → text) | P0 | S11, S12, S21 (skip rules), S13 (paste) |
| 13 | History dashboard, WPM/streak analytics | P1 | S30, S32 |
| 14 | Privacy mode (no storage) | P1 | S30 |
| 15 | Wake word ("Hey Flow") hands-free | P2 | S34 + R4 |
| 16 | Scratchpad / quick voice notes | P2 | S35 |
| 17 | Dev-tool awareness (Cursor/VS Code, file tags, variable names) | P2 | S23 profile for editors; R3 note |
| 18 | Cross-device sync of dict/snippets/styles | P2 | S33 (API is our sync surface) |
| 19 | Works across Mac/Windows/iPhone/Android | P2/P3 | S40a, S40 (mac), S41 (win, optional); phone via S33 API |
| — | *Not chased:* SOC2/HIPAA, teams/SSO/billing | — | local-first makes these moot |

Jake-specific features to **preserve** (beyond Wispr): TIMER route
(systemd-run), LOCAL route (Ollama qwen3:14b), media auto-pause/resume,
desktop notifications.

## Target architecture

Cargo workspace; engine-as-a-library; **protocol-first** design so the local
IPC and the stretch network API are the same contract over different
transports (this is the design decision the stretch goal forces today):

```
crates/
  dictate-proto      # serde types: commands, events, errors; versioned envelope   [S01]
  dictate-core       # pipeline engine: state machine, session orchestration      [S02]
  dictate-audio      # cpal capture, ring buffer, AGC, earcons, media-pause       [S10]
  dictate-vad        # silero VAD gating, auto-stop, silence trim                 [S11]
  dictate-stt        # SttProvider trait; whisper-rs impl; model manager          [S12]
  dictate-fmt        # rules layer + LLM layer (Ollama); eval harness             [S20,S21]
  dictate-dict       # personal dictionary + snippets + auto-learn                [S22,S24]
  dictate-context    # active-app detection; per-app profiles                     [S23]
  dictate-inject     # Injector trait; X11/Wayland/mac backends; paste-vs-type    [S13,S40a]
  dictate-hotkey     # evdev PTT (hold/release), toggle, cancel                   [S31]
  dictate-history    # rusqlite WAL + FTS5; analytics queries; privacy mode       [S30]
  dictated           # daemon binary: UDS JSON-RPC server, signals, systemd       [S02]
  dictate-cli        # control client: toggle/cancel/status/dict/history/tail     [S02]
  dictate-server     # axum HTTP+WS exposing dictate-proto over TCP (stretch)     [S33]
ui/                  # Tauri v2 app (TS): tray, settings, HUD, history, dict      [S32]
dictate/             # Python reference impl — stays until parity gate, then archived
```

Pipeline (superset of today's):

```
hotkey/signal/API → audio capture → VAD gate/trim → whisper-rs (CUDA)
  → corrections + dictionary bias → fmt rules → fmt LLM (context-aware, fail-open)
  → snippets → route (TYPE|TIMER|LOCAL|EDIT|COMMAND) → inject (paste/type) → history
```

### Locked decisions (change only with new evidence)

1. **whisper-rs (whisper.cpp) + CUDA** for STT; GGUF `large-v3-turbo` primary;
   CPU + `small`/`tiny` fallback for tests/no-GPU. (Feasibility doc, benchmarked.)
2. **Ollama over HTTP** stays the LLM runtime (qwen3 ladder). Embedding
   llama.cpp in-process is a later optimization, not v1.
3. **JSON-RPC over unix domain socket** for local control; **the identical
   message schema over WebSocket/HTTP** for the network API. One protocol crate.
4. **X11 first-class** (Jake's current setup; xdotool/xclip today). Wayland via
   adapter backends (wtype/virtual-keyboard, portal/libei) — R3 spike, not P0.
5. **Clipboard-paste as primary injection** with save/restore (already proven);
   per-app override to keystroke-typing for terminals/paste-hostile apps.
6. **SQLite (WAL) + FTS5** for history/analytics; schema extends `sql/schema.sql`.
7. **Tauri v2** for UI; daemon never depends on UI.
8. **Side-by-side migration**: new daemon uses distinct PID/socket/DB paths until
   cutover; `dictate-toggle`/`dictate-cancel` keep working (signal shim + CLI).

### Latency budget (p50, ≤10s utterance, RTX 5080)

| Stage | Budget | Notes |
|---|---|---|
| Capture flush + VAD trim | 50–100ms | trim silence before STT (net win) |
| whisper.cpp turbo CUDA | 300–500ms | measured projection; benchmark in S12 |
| Rules layer | ~0ms | pure Rust |
| LLM format pass | 200–400ms | **skip if < min_words or route≠TYPE-prose**; ladder 0.6b→4b |
| Injection (paste) | 50–100ms | clipboard save/restore |
| **Total** | **≤1.0s** | vs 0.87s Python today; parity with Wispr feel |

Every stage records duration into history (S30) so regressions are data, not vibes.

---

## The Slice Map

Legend — **Class**: architect (opus/xhigh) · implementer (sonnet/high) ·
lookup_fast (haiku). **Size**: S/M/L (≈ half-day / 1–2 day / multi-day worker
sessions). Each slice ships independently behind config flags; each slice's
worker session produces its own detailed plan + verification manifest
(automated-first per harness discipline).

### Wave 0 — Foundation (sequential)

**S00 — Repo reconciliation & workspace scaffold** · implementer · M · **DISPATCHED 2026-08-07**
Goal: single source of truth for the Rust code; workspace layout above.
Scope, as actually dispatched (three tasks, in order):
1. **Restore `dictate/` from `origin/master`** as a standalone commit — undoes
   `5e16667`'s collateral deletion; HARD constraint (Jake's daily driver).
2. **Workspace split** of the 13 modules into *only* the crates populatable
   with existing code today: `dictate-audio`, `dictate-stt`, `dictate-fmt`,
   `dictate-history`, `dictate-inject`, `dictate-core`, `dictated`. Explicitly
   **no placeholder crates** for `dictate-proto`/`-vad`/`-dict`/`-context`/
   `-hotkey`/`-cli`/`-server` — those are owned by S01/S11/S22/S23/S31/S02/S33
   and empty shells would create merge churn and false structure. Mechanical
   moves only; a module that resists a clean boundary stays in `dictate-core`
   with a `// TODO(S0x)` rather than an invented design.
3. **Handoff leftovers**: clippy clean, `cargo build --release`, systemd unit,
   `config.example.toml`, `scripts/run.sh`/`Makefile`, fresh `CLAUDE.md`
   (complementing the existing `AGENTS.md`), CUDA build env vars encoded in a
   durable target; commit `Cargo.lock`.
Verify: `cargo test --workspace` **≥78 green** (verified baseline, not the
stale 74); clippy clean; release build ok; binary still honors SIGUSR1/2.

**S01 — Protocol crate (`dictate-proto`)** · **architect** · M · ← keystone
Goal: the one message contract for IPC + network API + UI.
Scope: commands (StartDictation{mode}, Stop, Cancel, GetStatus, Config CRUD,
Dict CRUD, Snippet CRUD, HistoryQuery, TranscribeAudio{pcm|wav} for remote
clients); events (StateChanged, Partial, Final{text,route,timings}, Error,
AudioLevel); versioned envelope + capability handshake; serde JSON; binary
frame convention for audio chunks (defined now, used by S33).
Verify: round-trip serde tests for every type; schema doc generated; no
breaking-change without version bump (test pins golden JSON).

**✅ COMPLETE 2026-08-07 — reviewed and APPROVED by the Epic-lead** (commits
`b60e340` crate, `f3890e9` docs, `5f15051` lead-ordered fix). 225 workspace
tests green, clippy clean. Schema doc: `docs/protocol.md` (explicitly
subordinate to `crates/dictate-proto/tests/golden.rs`).
- **Envelope:** `{kind,v,id,command|result|error|event}` on streaming
  transports; `Command`/`CommandResult`/`Event` also usable bare so
  `POST /v1/transcribe` need not carry a correlation id. NDJSON framing on UDS.
- **Compatibility rule:** additive-only within `PROTOCOL_VERSION` — may add
  optional fields/variants/commands/events/capability flags; may never
  rename/remove/retype/re-nest. Deliberately asymmetric on unknowns: an
  unknown event/result degrades to `Unknown`, but an unknown **command fails**
  so the server answers `unsupported_command` instead of stranding a caller.
- **Timings** are four-state per stage — `Ran{ms}` / `Skipped{reason}` /
  `Failed{ms,error}` / `NotReported` — so a skip-rule skip, a fail-open Ollama
  burning 3s, a sub-ms rules pass, and no-data all stay distinguishable.
  `total_ms` is reported, not derived; the gap over the stage sum is signal.
- **`InjectionOutcome` is 8 variants, not a bool**, per R3. `AwaitingConsent`
  is non-terminal, which required adding `Event::InjectionResolved`.

**Epic-lead decisions on S01's five flagged items:**
1. `InjectionResolved` event — **APPROVED.** A non-terminal `AwaitingConsent`
   needs a resolution event; adding it after S33 would have been breaking.
2. `allows_route` empty-list-means-permissive — **OVERRULED, fixed in
   `5f15051`.** It was fail-open on the exact path that runs `systemd-run`
   (`Route::Timer`), and both `Capabilities::default()` and any JSON omitting
   `routes` produced a fully-permissive set — while `Features` next to it is
   deny-by-default. Now `routes.contains(route)`: empty permits nothing.
   Wire format unchanged; 23/23 golden tests unaffected.
3. `Stop`/`Cancel` carry no session ownership — **ACCEPTED as-is.** S02 must
   verify ownership daemon-side. Safe to defer: an optional session-id field
   is an additive change under the stated compatibility rule, so S33 can add
   it when multi-client actually exists.
4. `raw_text` gated only by docs — **ACCEPTED, deferred to S33's security
   review.** A dedicated capability flag is additive, so it costs nothing to
   add later; flag it during the LAN-exposure review.
5. `docs/protocol.md` in a new top-level `docs/` — **ACCEPTED.** It is
   product documentation for S32/S33 implementers, not a thoughts artifact.

**S02 — Daemon skeleton: `dictated` + UDS server + CLI + state machine** · **architect** · L
Goal: replace signal-only control with a real control plane (signals kept).
Scope: tokio daemon hosting engine; state machine Idle→Recording→Transcribing
→Formatting→Injecting→(Error|Done) with cancel at any point; UDS JSON-RPC
serving `dictate-proto`; event broadcast (subscribe); SIGUSR1/2 shim → same
code path; `dictate` CLI (toggle, cancel, status, tail, dict, history);
systemd user unit; distinct runtime paths (`dictated.sock`, new PID file).
Verify: automated integration test drives full pipeline via UDS with mock STT;
`dictate-toggle` script still works (daemon-level check: state transitions in
event log); concurrent-command safety tests.

### Wave 1 — Core pipeline hardening (parallel after S02)

**S10 — Audio subsystem v2** · implementer · M
Scope: ring buffer + pre-roll (capture ~300ms before hotkey to stop
first-syllable clipping — Wispr-feel detail); input device selection +
hotplug recovery; software gain/AGC (whisper-quiet speech, feature #11);
earcons on start/stop/cancel/error via rodio (Wispr's chimes); port media
pause/resume (playerctl subprocess or `mpris` crate); mic-mute detection warn.
Verify: unit tests on ring buffer/AGC math; fixture-driven capture tests; daemon
check: earcon + pause events appear in event stream.

**S11 — VAD (`dictate-vad`)** · implementer · M
Scope: silero-VAD via `voice_activity_detector`/ort (CPU); gate: no-speech →
skip STT entirely (kills the known hallucination-on-silence bug class); trim
leading/trailing silence pre-STT (latency win); hands-free mode: auto-stop
after N ms trailing silence (feature #2); config thresholds.
Verify: golden WAV fixtures (speech/silence/quiet) → gate decisions; latency
delta measured in timings.

**S12 — STT engine v2 (`dictate-stt`)** · implementer (spec by architect notes here) · M-L
Scope: `SttProvider` trait (future cloud/remote providers slot in); whisper-rs
impl behind it; **model manager**: hf-hub GGUF download into `models/`,
checksum, `dictate model pull|list`; language pinning + auto-detect
(feature #10); `initial_prompt` vocabulary-bias hook (consumed by S22);
no-speech threshold tuning; per-call timings; keep 23 correction pairs;
CPU/tiny fallback feature flag for CI. Known build env vars documented in
handoff §Learnings — bake into `build.rs` docs + `justfile`.
Verify: fixture WAVs → expected transcripts (tiny model, CPU, in CI); CUDA
smoke test target for the 5080 box; cold-load ≤5s (daemon check).
**R1 inputs (adopt):** chunked sha256 verify-then-delete-on-mismatch so a
corrupt/partial GGUF forces a clean retry; hf-hub revision-pinned commit SHAs
for reproducible, CDN-immutable pulls. **Adapt:** static catalog manifest
(stripped to our single whisper family); resumable Range-header download for
the non-hf-hub fallback path. **Avoid:** Handy's "no forced first-run
download" — we are CLI-first, so default-pull `large-v3-turbo` (or CPU
fallback) on first invocation rather than deferring to a UI wizard.
**R2 hook:** S12 MUST record real turbo-CUDA p50/p95 decode latency into the
timings table — that measurement is the gate that converts R2's streaming
DEFER into a GO/NO-GO. Do not skip it.

**S13 — Injection v2 (`dictate-inject`)** · implementer · M
Scope: `Injector` trait; X11 backend hardening (arboard+enigo paste w/
clipboard save/restore; direct-typing fallback path); per-app policy
(paste|type|off) keyed by `dictate-context` profile — terminals default to
type; large-text chunking; failure surfacing (never silently drop text —
on inject failure, text goes to clipboard + notification). Wayland backend
stub behind trait (real impl gated on R3).
Verify: unit tests on policy resolution + chunking; Xvfb + xterm read-back
smoke test (automated); manual TUI items only for focus-dependent cases.
**R1 inputs (adapt):** runtime tool-availability probing with an X11/Wayland
split (xdotool for X11; wtype→kwtype→dotool→ydotool chain for Wayland), plus
KDE-Wayland detection that gates `wtype` off (no `zwp_virtual_keyboard` there).
**Note:** Handy's paste-transaction engine is macOS/Windows-only — there is
nothing to crib for X11 clipboard paste-with-save/restore; we build it ourselves.
**R3 trait constraint (design the trait for this NOW, even though X11 ships
first):** `Injector::inject()` must be **async** and must return a distinct
**"pending user consent"** outcome alongside success/failure — on GNOME/KDE
Wayland, injection goes through a consent-gated portal, so a synchronous
success/failure signature cannot represent reality and would force a painful
refactor later. Also: clipboard save/restore is *unsupported* on GNOME Wayland,
so the per-app `paste|type|off` policy must be able to resolve to `type` from a
backend capability probe, not only from user config.

### Wave 2 — The Wispr magic layer (parallel; this is where parity is won)

**S20 — Formatting rules layer** · implementer · M
Scope: pure-Rust deterministic pass: filler-word/stutter/restart removal
(feature #4), number/date/currency/email/url normalization, capitalization,
optional spoken-punctuation mode ("period", "new line", "no caps"), unit
tests for every rule; config toggles per rule.
Verify: exhaustive table-driven unit tests; zero-LLM path measurable.

**S21 — Formatting LLM layer + eval harness** · **architect** · L · ← highest-leverage slice
Scope: context-aware prompt templates (tone Formal/Casual/Very-casual ×
app-category from S23; feature #3/#5/#6); self-correction resolution
("actually…"); list/email structure detection; fail-open contract with
length-ratio guard (port exists in `src/grammar.rs`); skip rules (short
utterance, non-prose routes); model ladder config qwen3:0.6b→4b with latency
budget enforcement; **eval harness**: golden corpus (seed from real history DB
transcripts) of transcript→expected pairs run as `cargo test` fixtures against
recorded LLM outputs + optional live-Ollama integration behind feature flag —
prompt changes become regression-tested, not vibes.
Verify: eval-harness pass-rate threshold; p50 LLM latency in budget; fail-open
tests (Ollama down → raw text still lands).

**S22 — Personal dictionary + auto-learn (`dictate-dict`)** · implementer · M
Scope: store (SQLite table + CLI/proto CRUD); three application points:
(a) whisper `initial_prompt` bias (S12 hook), (b) post-STT phonetic/fuzzy
replacement (rapidfuzz/rphonetic), (c) term injection into S21 prompts;
auto-learn v1: frequency-mine history for repeated unknown proper nouns →
suggestions surfaced via CLI/UI (feature #7); per-app dictionary scoping.
Verify: fixture tests for phonetic matcher precision (no false replacements);
end-to-end fixture with biased term.

**S23 — App-context engine (`dictate-context`)** · implementer · M
Scope: active-window provider trait; X11 impl (x11rb: `_NET_ACTIVE_WINDOW`,
`WM_CLASS`, title); profile mapping config: app → {tone, format on/off,
inject policy, dictionary scope, snippet scope}; app-category defaults
(terminal/editor/browser-mail/chat) incl. editor category for feature #17;
Wayland provider stub (compositor IPC adapters listed in R3).
Verify: unit tests on matching/precedence; live X11 smoke (daemon check:
context events in stream).
**R3 trait constraint:** `ContextProvider` must treat **"no window context" as
a normal first-class value, not an error** — on GNOME Wayland, absent context
is the *baseline* state (it needs a user-installed Shell extension), not a
failure. Every consumer (per-app tone, inject policy, dictionary/snippet scope)
must therefore have a defined no-context default path.

**S24 — Snippets** · implementer · S
Scope: spoken-trigger → expansion post-STT ("insert work email"), variables
({date}, {clipboard}, {selection}); proto/CLI CRUD; per-app scope (feature #8).
Verify: table-driven expansion tests incl. variable resolution.

**S25 — Command mode (EDIT route, finally)** · implementer · M
Scope: dedicated hotkey/prefix; capture selection (X11 primary selection /
Ctrl-C fallback with clipboard restore); instruction + selection → qwen3:14b
(existing `local_executor` path) → replace selection via injector (feature #9);
safety: preview-notification mode option; never-raise contract preserved.
Verify: mocked-LLM integration test through daemon; selection round-trip
Xvfb test; manual item for real-app focus behavior.

### Wave 3 — Interface & reach (parallel)

**S30 — History v2 + analytics + privacy** · implementer · M
Scope: extend schema (per-stage timings, route, app context, model used,
word count); FTS5 search; analytics queries (WPM, words/day, streaks —
feature #13); privacy mode: no-store toggle + purge command (feature #14);
retention policy config; import tool from Python-era DB (optional flag);
prior art: `thoughts/shared/plans/2026-02-03-interaction-history-analytics.md`.
Verify: migration tests old→new schema; analytics query unit tests; privacy
mode leaves zero rows (daemon check).

**S31 — Hotkey service (`dictate-hotkey`)** · implementer · M
Scope: evdev-based global capture (works X11 *and* Wayland): true
**hold-to-talk** with release detection, toggle, cancel chords; `input`-group
permission docs + graceful degradation to WM-keybind/signal path (today's
flow keeps working); double-tap-to-lock hands-free (pairs with S11 auto-stop).
Verify: uinput-driven automated tests (inject synthetic key events, assert
daemon state transitions); permission-missing degradation test.
**R1 inputs (adopt):** single-owner-thread `HotkeyManager` + mpsc command
channel (avoids evdev cross-thread races); **release-grace debounce window** —
defer key-up briefly and cancel it if a matching key-down arrives, which is how
Handy absorbs X11 auto-repeat bursts that otherwise flap hold-to-talk. **Avoid:**
dynamic register/unregister of the cancel shortcut (unstable on Linux) — make
the cancel chord a statically-grabbed key.

**S32 — Tauri UI v1** · implementer · L (deps: S01, S02, S30, S22)
Scope: tray + settings editor (config.toml round-trip), history browser w/
search + analytics cards, dictionary & snippet managers, live HUD overlay
(recording state, audio level, partial/final text via event subscription);
onboarding checklist (deps, permissions, model download). X11 overlay
first; Wayland layer-shell noted as best-effort. TS per Jake's allowance.
Verify: UI talks only `dictate-proto` (contract tests); daemon runs headless
without it (CI job proves).
**R1 inputs (adopt):** overlay as a separate always-on-top, transparent,
undecorated, non-focusable `WebviewWindow` reused across states; throttled
(~30 FPS) `mic-level` event bridge for the audio-level meter; monitor-under-
cursor + DPI-scaled position math kept separate from the layering mechanism.
**Open design work — do not assume solved:** Handy never calls
`set_ignore_cursor_events`; it has NO true click-through, only
`focusable(false)`/`skip_taskbar`/always-on-top. S32 must design and verify
click-through itself. Adapt gtk-layer-shell only as the Wayland best-effort
fallback behind an X11-native primary path.

**S33 — Network API server (`dictate-server`) — the stretch goal** · **architect** (security) + implementer · M (cheap because S01 paid the cost)
Scope: axum: WS endpoint speaking `dictate-proto` (control + events + binary
audio frames), HTTP POST `/v1/transcribe` (WAV/PCM upload → full pipeline
minus injection → formatted text JSON — the "remote dictation processor");
bearer-token auth (config-generated); bind 127.0.0.1 default, LAN opt-in;
optional rustls; request size/rate limits; server-side sessions so a future
thin client (laptop, phone shortcut) just streams audio. Explicitly **server
only** — no client app in scope (per Jake). This also becomes the
dict/snippet **sync surface** (feature #18) for any future second machine.
Verify: automated: auth-rejection, WAV-upload→text integration test, WS
session lifecycle; security review checklist (bind default, token entropy,
no PII in logs when privacy mode).
Decision note (2026-08-07): phone client ambition confirmed (personal) —
protocol must stay thin-client-friendly (single-request transcribe endpoint,
chunked-audio WS, bearer auth); recommend Tailscale/WireGuard for off-LAN
rather than raw TLS exposure. Server only; clients out of scope.

**S34 — Wake word ("Hey Flow" parity)** · implementer · S · optional · **R4 verdict: BUILD, conditional**
Scope: always-on low-power listener → triggers hands-free session; strict
opt-in (always-on mic is a privacy posture change); config keyword.
**Engine: openWakeWord via `oww_rs` (Apache-2.0).** Porcupine is DROPPED —
it phones home for license validation, which is disqualifying here.
**Gate (do this FIRST, half-day):** benchmark openWakeWord vs livekit-wakeword
idle CPU on Jake's actual machine, gated behind the S11 Silero VAD.
**If measured idle cost > ~2–3% of one core, STOP and defer S34** — no
independent idle-CPU figure exists for any engine, so this must be measured,
not assumed.
Verify: fixture-audio detection tests; false-positive rate logged; idle-CPU
measurement recorded as a daemon-level check.

**S35 — Scratchpad / voice notes** · implementer · S
Scope: route "note …" → append to notes store (no injection); retrieve via
CLI/UI/API (feature #16); markdown file or history-table flavor — decide in-slice.
Verify: route + store round-trip tests.

### Wave 4 — Platform expansion & shipping

**S40a — Platform abstraction layer** · **architect** · M · ← gate for mac/win
Scope: extract traits already implied: `Injector`, `ContextProvider`,
`HotkeyProvider`, `Notifier`, `MediaController`, `Earcons`; linux impls move
behind `#[cfg]`/features; kill any remaining `systemd-run`/`playerctl`
assumptions from core (TIMER route becomes linux-feature or gets mac impl
via `launchd`/`at`); CI builds `--no-default-features` cross-platform core.
Verify: core crates compile for `x86_64-apple-darwin`/`aarch64-apple-darwin`
(no linux deps leak); trait-mock test suite.

**S40 — macOS port** · implementer (after S40a) · L
Scope: whisper.cpp **Metal** feature build; injection: enigo CGEvent +
pasteboard w/ restore; hotkey: `global-hotkey`/CGEventTap + Accessibility &
Microphone permission onboarding flow; context: NSWorkspace frontmost app
(objc2); notifications: osascript/UNUserNotification via Tauri; launchd
user agent plist; document unsigned-app Gatekeeper steps (no notarization
requirement for personal use).
Verify: CI compile on macos runner + CPU-tiny STT fixture test; manual
checklist on real Mac hardware (list is short and explicit).

**S41 — Windows port** · implementer · L · **deferred until asked**
Scope sketch only: SendInput injection, WASAPI via cpal (free), Win32
foreground-window context, toast notifications, scheduled-tasks timer route.
Design-for (S40a traits), don't build.

**S42 — Packaging, CI, install story** · implementer · M
Scope: GH Actions matrix (arch-linux container + macos; CPU-tiny test tier);
release artifacts (cargo-dist or hand-rolled); AUR PKGBUILD; first-run model
download; `justfile` encoding the CUDA env-var build incantations; docs:
INSTALL.md per platform, migration-from-Python guide, LAN-API setup guide.
Verify: clean-VM install script smoke; CI green matrix.
**R1 inputs (adopt):** `build.rs` `$ORIGIN`-relative rpath so the binary finds
a co-located whisper.cpp shared lib without ldconfig; a CI staging script that
hard-fails when the CPU-fallback backend is missing (enforces locked decision
#1's fallback). **Adapt:** per-platform feature-flag shape and the GH Actions
release matrix — but substitute a CUDA toolkit step for Handy's Vulkan SDK.
**Avoid:** Handy has no CUDA path at all; its Vulkan/glslc env-vars do not
transfer. Our own documented CUDA incantations remain authoritative.

### Research spikes (cheap, early, parallelizable — run during Wave 0)

**R1 — Cannibalization audit: Handy (+ Whispering)** · **✅ COMPLETE 2026-08-07**
Artifact: `thoughts/shared/research/2026-08-07-r1-handy-cannibalization-audit.md`
(branch `rsi/76789b78`, commit `61661cf`). Handy audited at pinned commit
`b428ae4c`. **25 findings: 12 ADOPT / 6 ADAPT / 7 AVOID** across the five axes.
**No AVOID invalidates a locked decision** — the CUDA finding reinforces #1.
License: Handy is MIT; verbatim reuse of code/constants/scripts requires an
attribution NOTICE. Patterns/ideas carry no obligation. Per-slice actions are
propagated inline into S12/S31/S32/S13/S42 below.
Follow-ups (not blocking): Whispering + VoiceInk secondary passes were skipped
(time-boxed); true click-through is unsolved in Handy — open design work for S32.

**R2 — Streaming partials feasibility** · **✅ COMPLETE 2026-08-07 — verdict: DEFER**
Artifact: `thoughts/shared/research/2026-08-07-r2-streaming-partials-feasibility.md`
(commit `8ed09b8`). **Do not build live partials now.** If it ever graduates:
a single `large-v3-turbo` instance driven by a LocalAgreement-2-style
chunk-commit loop over whisper-rs `full()` (VAD-gated windows, prompt-token
continuity, no re-decode past the confirmed prefix) — NOT the naive
`stream.cpp` sliding window, NOT a two-model split.
**Killer risk:** whisper-rs exposes no state-reuse/incremental-decode API, so
every partial tick is a full mel→encoder→decoder pass contending with the
authoritative decode inside the same ≤1.0s budget. Secondary (reasoned, not
benchmarked): turbo's pruned 4-layer decoder likely flickers on short
repeatedly-reprompted windows, making the HUD actively distracting.
**Revisit trigger: after S12 lands real turbo-CUDA numbers** — that sizes the
remaining GPU headroom and converts this DEFER into a real GO/NO-GO.
Unaffected either way: inject-at-end stays (matches Wispr); partials would be
a HUD affordance only, never injected text.

**R3 — Wayland injection & window-context, 2026 state** · **✅ COMPLETE 2026-08-07**
Artifact: `thoughts/shared/research/2026-08-07-r3-wayland-injection-context.md`
(branch `rsi/f24927f5`, commit `d8d1478`).
**Inject:** wlroots (Hyprland/sway/river) gets native no-prompt injection via
`wtype`/virtual-keyboard-v1; GNOME/KDE only get async, **consent-gated**
portal+libei (GNOME default since v45; KDE portal-only, less mature).
**Clipboard:** save/paste/restore workable on wlroots (`ext-data-control-v1`);
**unreliable/unsupported on GNOME** (no in-compositor API; portal Clipboard
still unshipped) — this directly threatens our locked decision #5
(clipboard-paste as primary injection) *on GNOME/Wayland only*. X11 unaffected.
**Context:** Hyprland/sway first-class IPC; KDE needs a scripted DBus service;
GNOME requires a user-installed unofficial Shell extension.
**Load-bearing trait constraint (see S13/S23):** GNOME's baseline for both
injection and context is *absent or consent-gated*, not merely degraded.

**R4 — Wake-word engine bake-off** · **✅ COMPLETE 2026-08-07 — verdict: BUILD-S34 (conditional)**
Artifact: `thoughts/shared/research/2026-08-07-r4-wake-word-bakeoff.md`
(commit `51bdd11`). Ranked: **1) openWakeWord** via `oww_rs` Rust wrapper
(Apache-2.0, mature, proven at Home Assistant scale) · 2) livekit-wakeword
(new, pure-Rust — worth a parallel spike) · 3) rustpotter (fallback only;
dormant since Oct 2023) · 4) **Porcupine — DROP**.
**License blocker (Porcupine):** requires a Picovoice AccessKey validated
against their servers (confirmed phone-home; `create()` hangs when firewalled)
and the free tier ended 2026-06-30 with no non-commercial path. Disqualifying
on principle for a fully-local product.
**Idle CPU: NOT measured on target hardware** — no engine has an independent
figure. Best anchor: openWakeWord runs 15–20 models concurrently in real time
on one Raspberry Pi 3 core, so cost on Jake's workstation is very likely
negligible, especially gated behind Silero VAD (<1ms/chunk).
**Condition:** S34 opens with a half-day local benchmark of openWakeWord vs
livekit-wakeword on Jake's machine; **fall back to DEFER-S34 if measured idle
cost exceeds ~2–3% of one core** — a P2/optional feature is not worth real
standing resource cost.

---

## Dependency DAG

```
S00 → S01 → S02 ─┬─ S10 ─ S11 ─┐
                 ├─ S12 ───────┤
                 ├─ S13 ───────┼─→ S20 → S21 ─┬→ v0.9 parity gate ─→ cutover
R1..R4 (anytime) ├─ S31 ───────┤   S22 ───────┤   (archive dictate/)
                 └─ S30 ───────┘   S23,S24,S25┘
S01 ─────────────→ S33 (stretch, anytime after S02; security review before LAN expose)
S30+S22+S02 ─────→ S32 (UI, anytime after deps)
S40a → S40 (mac) → S41 (win, deferred)      S42 rides along from Wave 1 onward
```

**Critical path to "feels like Wispr" (P0):** S00→S01→S02→{S10,S11,S12,S13}→S20→S21 with S22+S31 alongside. Everything else layers on.

## RSI dispatch plan

Execution model: an Epic-lead session owns this map; each slice = one child
worker session spawned via `rsi-rpc AgentSpawnChild` (or `/spawn_child`
directive), kind/model/effort per table. Research spikes are `Research` kind;
slices needing in-slice design are seeded `architect`, mechanical ones
`implementer`. Suggested spawn parameters:

| Slice | Kind | model= | effort= | Wave |
|---|---|---|---|---|
| S00 | Refactor | sonnet | high | 0 |
| S01 | Feature | opus | xhigh | 0 |
| S02 | Feature | opus | xhigh | 0 |
| S10,S11,S13,S20,S22,S23,S24,S25,S30,S31,S35,S42 | Feature/Task | sonnet | high | 1–3 |
| S12 | Feature | sonnet (opus plan pass) | high | 1 |
| S21 | Feature | opus | xhigh | 2 |
| S32 | Feature | sonnet | high | 3 |
| S33 | Feature | opus (design+sec) → sonnet | xhigh/high | 3 |
| S40a | Refactor | opus | xhigh | 4 |
| S40 | Feature | sonnet | high | 4 |
| R1–R4 | Research | sonnet (R4 haiku triage) | medium/high | 0 |

Per-slice worker contract: research→plan→implement within the slice's
session tree; verification items bucketed AUTOMATED / DAEMON / TUI-manual
per harness discipline; thoughts artifacts committed; no pushes until Jake's
gate.

## Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| whisper.cpp CUDA on Blackwell (sm_120) build friction | P0 latency | Build traps already solved once (env vars in handoff); pin working ggml rev; CPU fallback; benchmark in S12 before deleting Python path |
| whisper.cpp 2–3× slower than HF turbo (183ms→~500ms) | feel | Still ≤1s total; VAD trim claws back; R2 streaming later; provider trait allows swap |
| Small-LLM formatting quality (0.6b too dumb, 4b too slow) | parity core | S21 eval harness + model ladder + skip rules; measured, not guessed |
| Wayland injection/context fragility | future | X11 first (Jake's env); R3 spike; trait-isolated backends; clipboard+notify degradation |
| evdev permissions (input group) | hotkey UX | Documented degradation to WM keybinds/signals (today's flow) |
| Hallucination on silence (seen in prod) | trust | S11 VAD gate + no-speech threshold (S12) |
| Tauri overlay on Wayland | cosmetic | Fallback: tray + notifications only |
| LAN API exposure | security | localhost default, token auth, opt-in bind, security-review item in S33 |
| Scope creep vs parity | schedule | P0 column is the contract; P2/P3 need explicit go |
| whisper-rs maintenance (Codeberg migration) | supply chain | Provider trait; vendored fallback acceptable |

## Decisions recorded 2026-08-07 (Jake)

1. **X11 confirmed**; no Wayland migration planned. R3 stays a cheap
   future-proofing spike; Wayland backends in S13/S23 remain trait stubs.
2. **macOS after Linux parity** — Wave 4 ordering confirmed.
3. **Phone client ambition is real** (personal app, not a business). S33 must
   keep the wire protocol thin-client-friendly: single-request
   `POST /v1/transcribe`, chunked-audio WS session, simple bearer auth. For
   off-LAN use prefer Tailscale/WireGuard over raw TLS exposure; TLS stays
   optional in-scope. Client apps remain out of scope for this epic.
4. **STT stays hard-local-only.** (Clarified: "cloud STT fallback" = optionally
   shipping audio to a paid transcription API when local inference is
   unavailable. Rejected as default posture; the `SttProvider` trait keeps that
   door open at near-zero cost if ever wanted.)
5. **Both hotkey modes are first-class** in S31: evdev true hold-to-talk AND
   the current WM-keybind/signal toggle path. Neither is a "fallback".

## Still open (defaults in force until Jake overrides)

6. **History default**: keep storing everything (current behavior) with
   privacy-mode opt-in, or flip default?
7. **Windows**: confirm "deferred until asked" (S41).
8. **Timer/LOCAL routes**: confirm these stay P0-preserved through cutover.
9. **Model disk budget**: OK to keep 2–3 GGUF models cached (~4–6GB)?
10. **Python retirement**: agree cutover gate = P0 matrix green + 1 week of
    daily-driver use with timings ≥ parity?

## Definition of done — v1.0 parity gate

Demo script (all via the Rust daemon, Python stopped): hold PTT → speak with
"um"s and a mid-sentence correction → clean formatted text lands in editor
<1s after release; same utterance in a chat app comes out casual; "timer ten
minutes" still sets a timer; unknown name added to dictionary once, recognized
after; select paragraph → command-mode "make this more formal" → replaced;
`dictate history search <word>` finds it; WPM stat visible; privacy mode
leaves no row; `curl -F audio=@clip.wav :7845/v1/transcribe` (LAN box) returns
the formatted text. Every item maps to a P0/P1 slice's verification bucket.

## Successor kickoff prompt (Epic-lead session)

Recommended spawn: **opus @ xhigh effort**, lead permissions. Paste verbatim:

> You are the Epic-lead orchestration agent for the dictate-agent → Wispr Flow
> parity build, running with lead permissions inside the RSI harness. The plan
> already exists — do not re-derive it. Sources of truth, in order:
> (1) `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md`
> — architecture, 25 slices, dependency DAG, dispatch table, locked decisions,
> parity gate; (2) `thoughts/shared/handoffs/general/2026-08-07_20-17-23_wisprflow-parity-epic-lead-handoff.md`
> — prior-session state and action items; (3) `thoughts/shared/handoffs/general/2026-04-10_17-51-32_rust-rewrite-implementation.md`
> — whisper-rs/CUDA build traps.
>
> Your job is dispatch and supervision, not implementation: spawn one child
> worker session per slice via RSI control surfaces (`rsi-rpc AgentSpawnChild`;
> directive fallback if unavailable) using the slice map's dispatch table
> (kind/model/effort per slice). Sequence: Wave 0 serial — S00 (Refactor,
> sonnet/high) → S01 (Feature, opus/xhigh) → S02 (Feature, opus/xhigh) — with
> research spikes R1–R4 (Research, sonnet/medium) fanned out in parallel with
> S00. Personally review S01's protocol output before S02 starts: it is the
> keystone contract shared by local IPC, the network API, and the UI. After
> S02, open the Wave 1 fan-out per the DAG.
>
> Constraints (HARD): workers follow the RPI worker contract (research→plan→
> implement, verification buckets, thoughts commits); NEVER `git push` — Jake
> pushes after his verification gates; keep the Python daemon (`dictate/`)
> untouched and runnable until the v1.0 cutover gate; the new daemon uses
> distinct socket/PID/DB paths. Escalate to Jake only at: S01 protocol
> approval, wave completions, Q6–Q10 decisions when a slice forces one, any
> LAN-exposure/security decision, and the v0.9/v1.0 parity gates. Track child
> progress via `AgentGetProgress`; file durable follow-ups via
> `AgentCreateIssue`.

## References

- Wispr Flow: [wisprflow.ai](https://wisprflow.ai/) — feature claims verified 2026-08-07
- Reviews (2026 feature confirmations: command mode, wake word, scratchpad, snippets, per-app styles):
  [efficient.app](https://efficient.app/apps/wispr-flow) · [droidcrunch](https://droidcrunch.com/wispr-flow-review/) · [max-productive](https://max-productive.ai/ai-tools/wispr-flow/) · [spokenly](https://spokenly.app/blog/wispr-flow-review) · [bossai](https://bossai.tech/blog/wispr-flow-review)
- Open-source landscape: [Handy](https://github.com/cjpais/Handy) (MIT, Rust+Tauri — R1 target) · Whispering · VoiceInk · [openalternative.co roundup](https://openalternative.co/alternatives/wisprflow)
- In-repo: feasibility research (2026-04-10), rewrite plan (2026-04-10), implementation handoff (2026-04-10), analytics plan (2026-02-03), `sql/schema.sql`, `src/*.rs`
