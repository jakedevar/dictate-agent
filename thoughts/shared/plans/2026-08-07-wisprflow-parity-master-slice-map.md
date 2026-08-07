---
date: 2026-08-07T12:00:00-05:00
researcher: Claude (RSI master orchestration session)
git_commit: 26dbf21cce8a4607adc7edf1d603d2ba584a3d74
branch: rsi/bb349467
repository: dictate_agent
topic: "Wispr Flow parity — master orchestration slice map (Rust rebuild)"
tags: [plan, orchestration, slice-map, rust, wispr-flow, whisper, tauri, api-server, parity]
status: draft_for_review
last_updated: 2026-08-07
last_updated_by: Claude (RSI master orchestration session)
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
| Python daemon (reference impl) | Working, in daily use | `dictate/` (2,304 LOC, 13 modules) |
| **Rust port, Phases 1–7** | Code-complete vs Python daemon; builds; **74 tests pass**; clippy/deploy artifacts outstanding | `Cargo.toml` + `src/` (2,783 LOC, 12 modules) |
| Rewrite feasibility research + real benchmarks | Final: Rust + whisper-rs decision locked | `thoughts/shared/research/2026-04-10-rust-go-rewrite-feasibility.md` |
| 7-phase rewrite plan | Executed | `thoughts/shared/plans/2026-04-10-rust-rewrite.md` |
| Implementation handoff (open items list) | Phases done, cleanup tasks listed | `thoughts/shared/handoffs/general/2026-04-10_17-51-32_rust-rewrite-implementation.md` |
| History analytics plan | Prior art for S30 | `thoughts/shared/plans/2026-02-03-interaction-history-analytics.md` |
| Real perf data (3,595 interactions) | avg transcription 0.183s (HF/5080), avg total pipeline 0.87s; whisper.cpp projected 0.4–0.6s | feasibility doc §Whisper Performance |
| whisper-rs build traps | Documented: `WHISPER_DONT_GENERATE_BINDINGS=1`, `PATH="/opt/cuda/bin:$PATH"`, API deltas from 0.14→0.16 | handoff §Learnings |

**Repo-state discrepancy to reconcile in S00:** `CLAUDE.md` claims the Rust
port lives archived in `src_rust_archive/` with Python active; on this branch
the Rust port is live at `src/`. Whichever is true on `master`, S00 promotes
the Rust code to a workspace and keeps Python running side-by-side until
cutover (do not delete `dictate/` until v1.0 parity gate passes).

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

**S00 — Repo reconciliation & workspace scaffold** · implementer · M
Goal: single source of truth for the Rust code; workspace layout above.
Scope: resolve `src/` vs `src_rust_archive/` vs `master`; split the existing
12 modules into workspace crates (mechanical moves, keep all 74 tests green);
finish handoff leftovers (clippy clean, `cargo build --release`, systemd unit,
`config.example.toml`, `scripts/run.sh`, CLAUDE.md rewrite); commit `Cargo.lock`.
Verify: `cargo test` ≥74 green in workspace; `--check` binary runs; clippy clean.

**S01 — Protocol crate (`dictate-proto`)** · **architect** · M · ← keystone
Goal: the one message contract for IPC + network API + UI.
Scope: commands (StartDictation{mode}, Stop, Cancel, GetStatus, Config CRUD,
Dict CRUD, Snippet CRUD, HistoryQuery, TranscribeAudio{pcm|wav} for remote
clients); events (StateChanged, Partial, Final{text,route,timings}, Error,
AudioLevel); versioned envelope + capability handshake; serde JSON; binary
frame convention for audio chunks (defined now, used by S33).
Verify: round-trip serde tests for every type; schema doc generated; no
breaking-change without version bump (test pins golden JSON).

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

**S13 — Injection v2 (`dictate-inject`)** · implementer · M
Scope: `Injector` trait; X11 backend hardening (arboard+enigo paste w/
clipboard save/restore; direct-typing fallback path); per-app policy
(paste|type|off) keyed by `dictate-context` profile — terminals default to
type; large-text chunking; failure surfacing (never silently drop text —
on inject failure, text goes to clipboard + notification). Wayland backend
stub behind trait (real impl gated on R3).
Verify: unit tests on policy resolution + chunking; Xvfb + xterm read-back
smoke test (automated); manual TUI items only for focus-dependent cases.

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

**S32 — Tauri UI v1** · implementer · L (deps: S01, S02, S30, S22)
Scope: tray + settings editor (config.toml round-trip), history browser w/
search + analytics cards, dictionary & snippet managers, live HUD overlay
(recording state, audio level, partial/final text via event subscription);
onboarding checklist (deps, permissions, model download). X11 overlay
first; Wayland layer-shell noted as best-effort. TS per Jake's allowance.
Verify: UI talks only `dictate-proto` (contract tests); daemon runs headless
without it (CI job proves).

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

**S34 — Wake word ("Hey Flow" parity)** · implementer · S · optional, after R4
Scope: always-on low-power listener (openWakeWord/rustpotter per R4 verdict)
→ triggers hands-free session; strict opt-in (always-on mic is a privacy
posture change); config keyword.
Verify: fixture-audio detection tests; false-positive rate logged.

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

### Research spikes (cheap, early, parallelizable — run during Wave 0)

**R1 — Cannibalization audit: Handy (+ Whispering)** · implementer · S
[Handy](https://github.com/cjpais/Handy) is MIT, Rust+Tauri, whisper-rs,
Linux/mac/win — the closest existing thing to this plan. Audit for: hotkey
handling per-platform, overlay/HUD approach, model manager UX, Wayland/mac
workarounds, packaging. Use `/cannibalize_research` against a clone. Output:
adopt/adapt/avoid list feeding S12/S13/S31/S32/S40.

**R2 — Streaming partials feasibility** · implementer · S
whisper.cpp stream-mode quality on turbo GGUF; verdict gates a future S1x
"live partials in HUD" slice (inject-at-end stays regardless — matches Wispr).

**R3 — Wayland injection & window-context, 2026 state** · implementer · S
wlr virtual-keyboard, `wtype`, xdg-desktop-portal RemoteDesktop, **libei**
maturity; compositor IPC (Hyprland/sway) for active-window; verdict shapes
S13/S23 Wayland backends. (Jake is on X11 today — this is future-proofing.)

**R4 — Wake-word engine bake-off** · lookup_fast→implementer · S
openWakeWord (ONNX) vs rustpotter vs Porcupine (license!); CPU cost while
idle; verdict gates S34.

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

## Open questions for Jake (answers reshape priorities, not architecture)

1. **Display server**: confirm X11 today (xdotool implies yes). Is Wayland
   migration on your horizon? → sets R3/S13-Wayland priority.
2. **Mac hardware**: Apple Silicon available for S40 testing? Mac needed
   early, or after Linux parity? (Current map: after.)
3. **Cloud STT fallback**: acceptable as opt-in provider (Deepgram/OpenAI) or
   hard-local-only forever? (Map assumes local-only; trait keeps door open.)
4. **LAN API**: plain bearer-token over LAN OK, or want TLS from day one?
   Any future phone-client ambition (shapes S33 session design)?
5. **Hotkeys**: keep current WM-keybind+signal flow as primary, or move to
   evdev true hold-to-talk (S31) as primary? Both stay supported.
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

## References

- Wispr Flow: [wisprflow.ai](https://wisprflow.ai/) — feature claims verified 2026-08-07
- Reviews (2026 feature confirmations: command mode, wake word, scratchpad, snippets, per-app styles):
  [efficient.app](https://efficient.app/apps/wispr-flow) · [droidcrunch](https://droidcrunch.com/wispr-flow-review/) · [max-productive](https://max-productive.ai/ai-tools/wispr-flow/) · [spokenly](https://spokenly.app/blog/wispr-flow-review) · [bossai](https://bossai.tech/blog/wispr-flow-review)
- Open-source landscape: [Handy](https://github.com/cjpais/Handy) (MIT, Rust+Tauri — R1 target) · Whispering · VoiceInk · [openalternative.co roundup](https://openalternative.co/alternatives/wisprflow)
- In-repo: feasibility research (2026-04-10), rewrite plan (2026-04-10), implementation handoff (2026-04-10), analytics plan (2026-02-03), `sql/schema.sql`, `src/*.rs`
