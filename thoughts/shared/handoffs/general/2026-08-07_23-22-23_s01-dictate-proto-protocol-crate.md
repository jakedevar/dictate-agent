---
date: 2026-08-07T23:22:23+00:00
researcher: Claude Opus 5
git_commit: f3890e9e0432843bcb006900f50274277c1d428c
branch: rsi/c25bc78e
repository: dictate_agent
topic: "S01 — dictate-proto: the protocol crate"
tags: [implementation, feature, protocol, serde, s01, wisprflow-parity, keystone]
status: complete
last_updated: 2026-08-07
last_updated_by: Claude Opus 5
type: implementation_strategy
---

# Handoff: S01 — `dictate-proto`, the protocol crate

## Original Request

Wave 0 keystone slice of the dictate-agent → Wispr Flow parity rebuild:
build `crates/dictate-proto`, the single message contract shared by three
consumers built by three later slices — S02 (UDS JSON-RPC control plane),
S33 (LAN WebSocket + `POST /v1/transcribe`), and S32 (Tauri UI). Types and
(de)serialization only: no transport, no tokio, no axum, no I/O. Design
first, then implement.

## Base-state self-check (passed, before any work)

All four conditions confirmed against S00's landed tree:

- `crates/` contains exactly `dictate-audio`, `dictate-core`, `dictate-fmt`,
  `dictate-history`, `dictate-inject`, `dictate-stt`, `dictated` ✅
- no top-level `src/` ✅
- `dictate/` present with 13 `.py` files ✅
- `cargo test --workspace` → **78 passed, 0 failed** ✅

## Stage contract

### Inputs

- Static: `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md`
  (sections "Target architecture", "Locked decisions", "Latency budget",
  S01/S02/S32/S33, "Decisions recorded 2026-08-07"); S00's handoff
  `thoughts/shared/handoffs/general/2026-08-07_22-49-51_s00-repo-reconciliation-workspace-scaffold.md`.
- Discovery budget: located `RouteType` in `crates/dictate-core/src/router.rs`
  and `Interaction` in `crates/dictate-history/src/history.rs` to make the
  protocol's `Route` and `HistoryEntry` reflect real existing shapes rather
  than invented ones. Confirmed `base64 0.22.1` was already in `Cargo.lock`
  before adding it as a direct dependency.

### Process

New crate, no seam carved into existing crates: `dictate-proto` depends on
nothing in the workspace and nothing yet depends on it. Only edit outside the
new crate was adding the workspace dependency entries (and a CLAUDE.md
correction that my own change made necessary).

### Outputs

- `crates/dictate-proto/` — 13 source modules, 2 integration test files.
- `docs/protocol.md` — language-neutral schema reference.
- Workspace wiring in `Cargo.toml`; `CLAUDE.md` crate-list correction.

### Verify

224 workspace tests green (78 baseline + 146 new), clippy clean across
`--all-targets --workspace`, dependency tree confirmed at 3 direct deps with
no async runtime.

## Recent changes

1. `b60e340` — `feat: dictate-proto -- the one wire contract for IPC, network
   API, and UI (S01)` — the crate, 6038 insertions across 18 files.
2. `f3890e9` — `docs: language-neutral protocol reference for S32/S33 (S01)` —
   `docs/protocol.md` + the CLAUDE.md correction.

## Design decisions (the ones worth challenging)

### Envelope and versioning

`{"kind":"request|response|event","v":1,"id":…,…}`. `v` rides on **every**
message, not just the handshake, so a mid-stream version mismatch is
diagnosable. `Command`/`CommandResult`/`Event` are also usable **bare**, so
`POST /v1/transcribe` takes a plain command body and returns a plain result
rather than being forced to carry a correlation id it has no use for. NDJSON
framing for the UDS byte stream (safe: serde_json escapes newlines, so a raw
`\n` is always a delimiter — there is a test).

### Compatibility rule adopted

**Additive-only within a `PROTOCOL_VERSION`.** MAY add optional fields with
behavior-preserving defaults, enum variants, commands, events, error codes,
capability flags. MUST NOT rename/remove/retype/re-nest, or make an optional
field required. Enforced by `tests/golden.rs`, which pins both directions
(serialize → exact JSON, and that JSON → the same value).

Unknown handling is **deliberately asymmetric**, and this is the decision I
most want reviewed:

| Path | Behavior |
|---|---|
| unknown field | ignored everywhere (no `deny_unknown_fields`) |
| unknown open-enum value | round-trips **losslessly** via `serde(from/into = "String")` |
| unknown event / result | degrades to `Unknown` |
| unknown **command** | **fails to deserialize**; server owes `unsupported_command` |

Rationale: a client ignoring an event it does not understand loses nothing; a
server silently dropping a command strands the caller waiting forever for an
effect that will never happen. I used `from/into = "String"` rather than
`serde(other)` specifically so a v1 relay between two v1.1 peers cannot
flatten values it does not understand.

**Known limitation, documented in the crate docs:** `Event::Unknown` and
`CommandResult::Unknown` do *not* preserve their payload (serde's
`#[serde(other)]` only supports a unit variant). A relay must therefore
forward raw bytes rather than re-serializing a parsed value. Open enums do
not have this limitation.

### Timings (constraint 3)

Six stages — `capture`, `vad`, `stt`, `fmt_rules`, `fmt_llm`, `inject` — plus
reported (not derived) `total_ms` and `audio_ms`. Each stage is one of four:

- `Ran{ms: f64}` — fractional, because the rules layer is genuinely sub-ms and
  rounding it to 0 would hide it;
- `Skipped{reason}` — carries *why* (`below_min_words` vs
  `dependency_unavailable` are very different signals for the budget);
- `Failed{ms, error}` — the fail-open LLM case: Ollama burned 3s and we fell
  through, and those 3s are in the user's latency budget and must not vanish;
- `NotReported` — **the `Default`**, so a stage added by a future revision
  reads as no-data on an older peer rather than a fabricated zero.

`total_ms` is reported separately because it includes scheduling/queueing that
belongs to no stage, so it is normally *greater* than the sum — the gap is
itself the signal. `real_time_factor()` and `slowest_stage()` are provided for
S12.

### Consent-gated injection (constraint 5)

`InjectionOutcome` is a 8-variant enum, not a boolean. `AwaitingConsent` is
**non-terminal** — `Event::Final` may carry it with the resolution arriving
later. That forced one addition beyond the specified event list: a new
**`Event::InjectionResolved`**. Adding it later would have been breaking for
exhaustive matchers, which is exactly the bump the versioning discipline
exists to avoid, so I added it now. `Delivered` covers the thin-client case
(the caller took the text; nothing was injected anywhere) — a success, not a
skip and not a failure. X11 never produces `AwaitingConsent`, so this costs
nothing today.

### Partial vs Final (constraint 4)

Enforced structurally rather than by comment, in two places:

- **Rust**: `Hypothesis` and `FinalText` are distinct newtypes; `Hypothesis`
  has no `as_str()` and no conversion to `FinalText`.
- **Wire**: `partial` carries **`hypothesis`**, `final` carries **`text`**. A
  TS/Swift client reading `.text` off a partial gets `undefined` — the mistake
  fails visibly instead of typing a half-finished sentence.

`Event::injectable_text()` returns `Some` only for `Final`. Partials stay in
the schema per R2's DEFER, gated by `features.partial_transcripts` (false).

### Capabilities are per-connection, not per-server

The load-bearing idea: one daemon must offer injection on its UDS socket and
refuse it to a phone on the LAN. Every `Features` flag defaults to `false`
(fail-safe: a missing flag means "no button", never "may inject text").
`Capabilities::routes` gates routes as a *subset* — a remote client may get
`type` while being denied `timer` (which would run `systemd-run` on the host).

`Command::is_permitted(&Features)` lives in this crate rather than in S02 and
again in S33, so the local and LAN paths cannot drift — a capability enforced
on one transport and forgotten on the other is exactly the bug this crate
exists to prevent.

### Binary frame codec

20-byte header (`DCTA` magic, frame version, kind, flags, stream_id, seq,
explicit `payload_len`), little-endian. Format is declared **once** in
`begin_audio_stream`, never per frame — a 20ms chunk is 1280 bytes and
repeating rate/channels 50×/s would be overhead *and* would let a stream
contradict its own declaration. Explicit length means one encoding serves both
the message-framed WebSocket (`decode`) and the UDS byte stream
(`decode_prefix` / `decode_all`), and means an unknown frame kind can be
skipped without losing sync. `payload_len` is bounds-checked against 1 MiB
*before* allocating (attacker-controlled). `FRAME_VERSION` is versioned
independently of `PROTOCOL_VERSION`.

### Config is not modeled

`ConfigEntry{path, value}` — dotted path plus opaque `serde_json::Value`. The
trap avoided: mirroring `dictate-core::config::Config` here would make every
config key a later slice adds (S10 pre-roll, S11 VAD thresholds, S22
dictionary paths) into a protocol change, and would force a UI rebuild to
expose a setting the daemon already supports. Cost: the protocol cannot
type-check a setting; validation is the daemon's job → `config_invalid`.

## Learnings

- **`serde(flatten)` inside an internally-tagged enum works**, including
  through a `Box`. `Event::Final` flattens `Transcript` so the wire shape is
  the plan's specified `Final{text, route, timings}` rather than a nested
  object, and `CommandResult::Transcript` carries the identical payload. That
  shared shape is the concrete reason S33 is cheap — the HTTP response and the
  WebSocket event are the same fields and cannot drift. There is a test
  asserting field-by-field equality between the two paths.
- **`serde(other)` requires a unit variant**, which is why open enums use
  `from/into = "String"` instead. Worth knowing before anyone "simplifies" it.
- **Boxing for layout does not touch the wire.** `Transcript` is ~424 bytes
  (mostly `StageTimings`, because each `StageTiming` carries a possible
  `Unknown(String)`), which was inflating every `Event` — including
  `AudioLevel`, broadcast ~30×/s. Boxed `Transcript`, `Status`, `ServerHello`,
  and `ProtoError.detail`; the golden tests passing unchanged is the proof it
  was invisible on the wire.
- **`base64` was already in `Cargo.lock`** transitively, so the one
  non-serde dependency added zero new tree. Direct deps: 3. No tokio, no
  async, nothing that would make a Tauri UI or phone client think twice.

## Scope discipline

No existing crate's behavior was changed. `dictate-proto` depends on nothing
in the workspace and nothing depends on it yet. I did **not** need to change
another crate to make the protocol work — no design smell surfaced.

Two edits outside the new crate, both consequences of it rather than
refactors: the workspace `Cargo.toml` member/dependency entries, and a
CLAUDE.md correction (it listed `dictate-proto` under "crates NOT yet
created", which my own commit made false).

## VERIFICATION_ITEMS:

### AUTOMATED

All verification for this slice is automated. This crate is pure logic with no
I/O, so every claim is expressible as a test, and 146 were written.

- [x] Round-trip serde for every command, event, result, error, and record
      type — `command::tests::*`, `event::tests::every_event_round_trips`,
      `result::tests::every_result_round_trips`,
      `envelope::tests::every_message_kind_round_trips`.
- [x] Golden JSON pinning the exact wire form, checked **both** directions —
      `tests/golden.rs` (23 tests), including every open enum's full wire
      vocabulary and the binary frame layout byte-for-byte.
- [x] Unknown field / newer variant degrades per the stated rule —
      `tests/compat.rs` (15 tests), including the command-vs-event asymmetry
      asserted side by side, lossless open-enum relay, and fail-safe
      capability defaults.
- [x] Cancellation reachable from every non-terminal state —
      `state::tests::cancel_reachable_from_every_non_terminal_state`.
- [x] Forward-skip legal, backward illegal, terminals reset only to Idle —
      `state::tests::stages_may_be_skipped_forward_but_never_revisited`,
      `terminal_states_only_reset_to_idle`.
- [x] A stage that did not run is distinguishable from one that took 0ms —
      `timings::tests::zero_duration_is_distinct_from_not_run`.
- [x] A partial carries no `text` field on the wire —
      `event::tests::a_partial_carries_no_text_field_on_the_wire`.
- [x] Consent-gated injection has a complete, expressible event flow —
      `event::tests::consent_gated_injection_has_a_full_event_flow`.
- [x] A remote connection cannot inject text, drive the host mic, write
      config, or read history —
      `command::tests::a_remote_client_may_transcribe_but_not_drive_the_host`.
- [x] HTTP and WebSocket paths carry an identical transcript shape —
      `result::tests::the_http_and_websocket_paths_carry_the_same_transcript_shape`.
- [x] Binary frames: round-trip, truncation, bad magic, wrong version,
      oversized length rejected before allocation, concatenated decode,
      unknown kind keeps sync — `frame::tests::*` (15 tests).
- [x] `cargo test --workspace` → **224 passed, 0 failed** (78 baseline + 146).
- [x] `cargo clippy --all-targets --workspace` → **clean, zero warnings**.

### TUI manual

None. This crate has no terminal surface.

### Daemon-level

None. This crate has no runtime, no process, and no side effects; there is no
daemon state for a check command to observe. Daemon-level items for the
protocol belong to S02, which is what first puts these types on a socket.

## Open questions for the Epic-lead (things you may want to overrule)

1. **`Event::InjectionResolved` is an addition** beyond the slice map's
   specified event list (`StateChanged, Partial, Final, Error, AudioLevel`).
   Justified by R3's async consent gate — without it, `Final` cannot carry a
   terminal outcome on Wayland and adding the event later would be breaking.
   Overrule if you would rather fold the resolution into a `StateChanged`
   transition.
2. **`Capabilities::allows_route` treats an empty `routes` list as
   permissive**, for compatibility with a peer predating route gating. That is
   the *unsafe* default direction, chosen for consistency with "omitted means
   unspecified". S33 is documented as required to populate it explicitly. If
   you prefer fail-safe here (empty = deny nothing allowed), say so — it is a
   one-line change now and a breaking one after S33 ships.
3. **`Stop`/`Cancel` are gated on holding *any* session capability** rather
   than on session ownership, which the protocol cannot express. S02 must
   still verify the caller owns the session it is stopping.
4. **`raw_text` is exposed in `Transcript`** (pre-formatting STT output). It
   is the ground truth the formatting layers are evaluated against, and also
   the most privacy-sensitive field. Currently gated only by documentation
   ("present when the connection is permitted to see it"), not by a capability
   flag. Consider whether it deserves its own flag before S33.
5. **`docs/protocol.md` was placed in a new top-level `docs/`**, not under
   `thoughts/`. Rationale: it is a durable API reference S32/S33 implement
   against, not a research artifact. Move it if the repo convention should be
   otherwise.

## What S02/S32/S33 should read first

`docs/protocol.md` §13 (implementation checklist) and the crate-docs
compatibility rule in `crates/dictate-proto/src/lib.rs`. Then
`tests/golden.rs` — the goldens are the authority, and the doc explicitly
defers to them.
