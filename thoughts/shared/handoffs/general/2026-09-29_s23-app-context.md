---
date: 2026-09-29
slice: S23
base: ce3484e804d53f5c0ee371ead0994b8c2f3a3f12
status: partial-original-window-targeting
---

# S23 app context handoff

Implemented and verified the context engine, profiles, per-session decision,
policy injection handoff, capability-gated protocol discovery/events, config
and CLI. The original-window *delivery* guarantee still needs a backend change;
see the P1 follow-up below. Do not advertise that guarantee as implemented.

## Verification observed to completion

- Baseline: `cargo test --workspace --all-targets`: **424 passed, 0 failed**
  (`/tmp/S23-baseline.log`; first build 6m07s).
- Final: same workspace command: **447 passed, 0 failed**, 0 ignored
  (`/tmp/S23-test.log`). These are 23 additional tests.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed
  (`/tmp/S23-clippy.log`; initial native build 4m31s, final incremental 0.32s).
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- `cargo tree -p dictate-cli -e normal | rg 'whisper|dictate-fmt'`: empty.
  Also checked `dictate-context|x11rb`: empty. CLI remains protocol-only here.
- `cargo test -p dictate-context --all-targets --features x11-tests -- --nocapture`:
  **13 passed, 0 failed**, 0 ignored (`/tmp/S23-x11.log`). This repeats the 12
  ordinary context tests and adds the feature-gated live test, so do not add 13
  to the workspace count as though all were distinct.
- Feature-enabled context clippy with `-D warnings`: passed, including live test.
- Private Xvfb: 500 captures, debug build, **p50 0.021ms, p99 0.036ms,
  max 0.109ms**. Earlier runs also measured p99 0.031–0.047ms.
  Test creates synthetic windows/properties via x11rb, never reads `:0`,
  tests UTF-8 primary/Latin-1 legacy titles, PID/process name, absent EWMH focus,
  partial metadata, destroyed windows, and reuse after an error. Only absence
  of Xvfb skips it; startup/capture/property failures fail the test.

Direct launch instructions/worker contract required foreground tests and
forbade ending the turn to wait; followed those rather than the general RSI
catalog recommendation to dispatch durable jobs. No subagents, branch changes,
pushes, live-daemon actions, microphone capture, or private-data fixtures.

## Decisions and defaults

- `ContextProvider::capture() -> Option<WindowInfo>`: absence is normal.
  `NoContext` is selected for unset/empty DISPLAY or disabled context. Wayland
  compositor adapters are deferred. A configured app ID still resolves through
  profiles with no host title/instance when there is no live context.
- X11 uses one reusable RustConnection on one dedicated thread. Request queue
  capacity is one; `try_send` plus an 8ms deadline bounds the synchronous
  rendezvous at session acceptance. All X11 and `/proc` I/O is off async workers.
  Expired queued requests are dropped; errors clear the connection for a quiet
  reconnect. A hung display can occupy at most that one worker, never grow the
  blocking pool or hold up subsequent captures (which return None). Property
  reads are capped at 4096 bytes. No per-capture error logs for normal absence.
- Profiles compile regexes at config load (1MiB regex size limit), not per
  session. File order/first-match precedence; all specified match fields must
  match. Class/instance globs support `*` and `?`, are case-insensitive and
  anchored, and escape all other characters. Title uses regex syntax.
- Built-in class categories include every requested class and check instance
  when class is unknown. Browser Gmail/Outlook title rules precede class defaults;
  user profiles precede both. All categories use Neutral tone.
- Terminal defaults disable LLM formatting, spoken punctuation and line breaks,
  while **inheriting global injection** (ghostty Ctrl+V remains supported).
  Explicit profile values override category defaults; other unset bools/injection
  remain None to inherit global config.
- `ResolvedProfile` and open `ContextInjection` are additive protocol types so
  the CLI can render discovery without depending on context/inject/core.
  Its `context` is Option<AppContext> to represent the normal absent state.
  Unknown config values/invalid regexes/malformed types fail load with the
  profile name; unknown config keys warn once at load, also with the profile
  name where applicable. Added the new crate to dictated's tracing floor so
  those warnings are visible in the actual binary.
- Engine resolves context before `audio.start().await`. Pipeline publishes one
  ContextResolved before Transcribing. Per-call policy overrides preserve S13's
  capability downgrade and clipboard guarantees. Off skips before availability
  probing and the commit point, with Skipped{Disabled}/matching StageTiming.
  Caller delivery (`inject=false`) remains Delivered even for an Off profile.
- `GetContext` and ContextResolved events require `context_read`; local
  connections get it, remote/default capabilities do not. Filtering events is
  necessary because the event bus is shared across local/remote subscribers.
  History saves only lowercase stable app ID, never the title. Both per-session
  and global privacy leave no history rows.

## Integrator wiring (parallel slice seams)

- S20: `ResolvedOptions.context: Option<AppContext>` and
  `ResolvedOptions.profile: ResolvedProfile` carry the immutable decision.
  Feed context/tone into FormatContext; consume `profile.llm_format`,
  `spoken_punctuation`, and `spoken_line_breaks` against global defaults.
  S23 intentionally did not edit dictate-fmt or the formatting section.
- S03: uploads must set `capture_context=false` even for a local connection,
  then call `Pipeline::resolve_context(&mut options)` before starting the
  upload pipeline. Remote capability resolution already sets it false.
  Keep ContextResolved publication before Transcribing in the unified run path.
- `TextInjector::inject` now takes `Option<InjectionPolicy>` as its second
  argument. None inherits output.policy; all adapters/doubles in this base
  were updated. Core re-exports InjectionPolicy for callers.
- Both ResolvedOptions.context and profile.context are populated together by
  resolve_context; do not resolve again after recording/formatting or mutate
  either to follow later focus changes.
- Shared-file conflict risks: config aggregation, Cargo files, protocol golden
  tail/remote capability golden, CLI command table/renderer, daemon pipeline
  assembly, engine start/resolve_options, server GetContext/filter arms, and
  harness Setup/Pipeline construction. pipeline.rs edits are limited to
  ResolvedOptions/session context/history app ID/injection; formatting region
  was not moved, reindented or edited.
- Narrow incidental file outside listed ownership: dictate-hotkey/src/lib.rs
  receives one ContextEngine::disabled() field in its test Pipeline literal;
  dictated/src/main.rs receives one logging target for the new crate. Existing
  harnesses use disabled context so no ordinary test reads the real desktop.

## Follow-up required for original destination guarantee

RSI **Issue #1065**, UUID `7d9fd6eb-ed6f-550e-a1ce-19673db5f6b1`, P1:
`dictate-agent: preserve the session's original injection destination when focus moves`.

The S23 snapshot/profile remains immutable when focus moves, proven by the
control-plane double. However S13's existing Enigo injector sends keys to the
currently focused window. Snapshotting metadata cannot redirect that delivery.
The integration contract restricts S23 to the policy port/HostInjector policy
selection; backend target identity/focus activation was not changed here.
Manager must assign the follow-up or extend ownership before claiming the
original-window delivery intent. It needs an opaque local window identity,
focus-change/destroyed-target refusal or safe targeting, isolated Xvfb tests,
and preserved cancellation/clipboard guarantees. No additional product choice
is inferred: the prompt already requires original-destination semantics.
