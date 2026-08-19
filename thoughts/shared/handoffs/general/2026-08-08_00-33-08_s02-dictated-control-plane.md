---
date: 2026-08-08T00:33:08+00:00
researcher: Claude Opus 5
git_commit: 829c6fe5e23ce882e199a5477c28545b1d0d99ad
branch: rsi/c0ce10fb
repository: dictate_agent
topic: "S02 — dictated daemon skeleton: UDS control plane, state machine, and dictate CLI"
tags: [implementation, feature, daemon, control-plane, state-machine, concurrency, cancellation, s02, wisprflow-parity, wave-0]
status: complete
last_updated: 2026-08-08
last_updated_by: Claude Opus 5
type: implementation_strategy
---

# Handoff: S02 — `dictated` control plane, state machine, and `dictate` CLI

## Original Request

Final Wave 0 slice of the dictate-agent → Wispr Flow parity rebuild. Replace
the signal-only daemon with a real control plane while keeping the signal path
working: a cancellable state machine emitting `dictate-proto` events, a tokio
daemon serving JSON-RPC over a unix socket with event subscription, a signal
shim routed through the *same* code path as the protocol commands, a `dictate`
CLI, a systemd unit, and runtime paths that never collide with the retained
Python daemon. Concurrency and cancellation semantics were called out as the
hard part and the design centerpiece.

## Base state (verified before starting)

`crates/` held the eight expected crates, `dictate/` had 13 `.py` files, and
`cargo test --workspace` reported **225 passed, 0 failed**. Base state good.

## What shipped

| Commit | Contents |
|---|---|
| `61faf4a` | `dictate-core`: engine, state machine, cancellation, pipeline, ports, event bus; `dictate-history` read path |
| `be845c0` | `dictated`: UDS server, signal shim, runtime paths, 37 integration tests, systemd unit |
| `a92df17` | `dictate-cli`: the `dictate` binary |
| `829c6fe` | Docs (`CLAUDE.md`, `docs/protocol.md` correction) and build wiring |

**Tests: 379 passing, 0 failed** (base 225 → **+154**). Clippy clean across
`--all-targets --workspace`. Release build produces `dictated` (45MB) and
`dictate` (1.4MB).

## The four decisions worth reviewing

### 1. A single-writer actor, not a lock

Every hard case in this slice is a race between two commands. A
`Mutex<DaemonState>` makes each individually safe and collectively
unanalyzable, because the question is never "is this field torn" but "which
happened first".

So all commands — from every connection *and* from the signal handler — go
down one mpsc channel into one task (`dictate-core/src/engine.rs`). Ordering is
the channel's ordering. "What if these race" has one answer: one of them is
second, and sees what the first left. The engine never blocks on the pipeline —
a session runs in its own task and reports back through the same mailbox — so
a `Cancel` is processed while transcription is still in flight.

Two racing `StartDictation` commands: exactly one wins, the other gets `busy`
with `retry_after_ms: 500`. Tested with 8 concurrent connections.

### 2. Cancellation has an explicit commit point

Injection is physically irreversible. A "cancelled?" flag checked at stage
boundaries races it — cancel and inject can *both* win, leaving text pasted in
the user's editor **and** a `cancelled` event claiming it never happened.

`CancelToken` (`dictate-core/src/cancel.rs`) makes `cancel()` and
`enter_commit()` the two exits from one compare-exchange, so exactly one wins:

```
           cancel()                 cancel() -> AlreadyCancelled
     Live ──────────► Cancelled ──────────────────────────────►
       │
       │ enter_commit() (Some(guard))
       ▼
  Committed ──────────────────────────────────────────────────►
           cancel() -> TooLate
```

A cancel that loses is answered `conflict` (an existing protocol code), never a
false success. The refusal window is only as wide as the injector call itself;
every other stage interrupts at its next checkpoint. Asserted under 2,000
rounds of thread contention that `Accepted` and `committed` can never both hold.

Cancellation is reachable from `recording`, `transcribing`, `formatting`, and
`injecting` (pre-commit) — one test per state, each asserting nothing was
injected and the capture stream was released.

### 3. Session ownership is capability-derived, so S33 inherits it

`Stop`/`Cancel` carry no session id (S01 item 3, Epic-lead accepted), so the
daemon decides. The rule keys on `host_capture` — precisely the permission to
switch on *this host's* microphone — rather than on the transport:

| Actor | May control |
|---|---|
| `Actor::Signal` | anything — it is the host's physical control surface |
| connection with `host_capture` | its own session, or an unowned host session |
| connection without `host_capture` | only its own |

A phone on the LAN (`remote_transcription_only`, no `host_capture`) can never
stop or cancel the desktop's dictation. That property is tested today.

**A disconnect orphans rather than cancels.** `dictate toggle` is a keybinding:
it connects, sends one command, and exits, so the connection that *starts* a
session is almost always gone before the connection that stops it exists. If a
disconnect cancelled, the CLI could not work at all. A session owned by a
connection without `host_capture` *is* cancelled on disconnect — unreachable
today, and there for S33's upload sessions.

### 4. The signal shim shares the command path

`signals.rs` contains no recording logic. SIGUSR1 builds `Actor::Signal` and
calls `EngineHandle::toggle`, which dispatches to the same `handle_start` /
`handle_stop` that `start_dictation` and `stop` reach. Proven by a test that
drives one dictation through the socket and one through the shim and asserts
the state-transition sequences are **equal**, plus a test that raises a real
kernel-delivered SIGUSR1/SIGUSR2 at the process.

Toggling *inside* the engine task is also what makes the signal path race-free.

## Two production bugs the tests found

1. **Lost wakeup on `Stop`.** `Notify::notify_waiters()` drops the signal if the
   waiter has not parked yet. The session task parks only after raising the
   recording notice and pausing media, so a `Stop` landing in that window — a
   push-to-talk tap — hung the session in `recording` until daemon restart.
   Fixed with `notify_one()` (permit semantics) plus `Notified::enable()` in
   `CancelToken::cancelled()`, which registers before the flag check. Regression
   tests loop 25× each.
2. **`kill(pid, 0)` and PID 0.** POSIX defines pid 0 as *the caller's whole
   process group*, so the probe returned "alive" for a zeroed PID file — which
   would have locked the real daemon out of its own socket permanently.

## Python side-by-side: paths no longer collide

Both daemons previously wrote `~/.config/dictate-agent/dictate.pid` and opened
the same `history.db`. Now:

| | `dictated` | Python |
|---|---|---|
| socket | `$XDG_RUNTIME_DIR/dictate-agent/dictated.sock` | — |
| PID | `$XDG_RUNTIME_DIR/dictate-agent/dictated.pid` | `~/.config/dictate-agent/dictate.pid` |
| DB | `$XDG_DATA_HOME/dictated/history.db` | `~/.local/share/dictate-agent/history.db` |

`scripts/dictate-toggle` and `scripts/dictate-cancel` are **byte-identical** —
verified, `git status` shows them untouched, and `dictate/` was not modified at
all. They keep working because `dictated` takes the legacy PID file
**claim-if-free**: only when nothing live holds it, released only if it still
owns it. Python running → scripts drive Python (correct, it owns it).
`dictated` alone → scripts drive `dictated`. Neither ever overwrites a PID file
a living process owns.

## Protocol gaps hit (Epic-lead decisions, NOT taken unilaterally)

1. **No `toggle` command.** The protocol has `start_dictation` and `stop`, so
   `dictate toggle` must `get_status` then act, leaving a TOCTOU window. The
   daemon closes it for signals (toggle resolves inside the engine task) but a
   socket client cannot borrow that. Consequence is bounded: the CLI reports
   `busy`/`no_active_session` and does not retry. Jake's hotkey path is
   unaffected. **Adding `toggle` would be additive** under the compatibility
   rule (new command, no version bump) — recommend it, but S32/S33 share this
   contract so it is your call.
2. **`docs/protocol.md` was stale and I corrected it.** It said an empty
   `routes` list "means unspecified and is read permissively" — false since
   `5f15051`. The doc declares itself subordinate to the golden tests, so this
   is a doc fix, not a protocol change. Flagging it because S32/S33 implementers
   may have already read the fail-open version.
3. **`conflict` for a post-commit cancel** is a semantic choice, not a protocol
   change (existing code). The protocol says cancellation is reachable from
   every non-terminal state; it is, right up to the commit point. Refusing after
   that is the only way to satisfy "must not leave a half-injected result".

## Scope discipline

Not built, as instructed: network/LAN server (S33), Tauri UI (S32), VAD (S11),
hotkey service (S31), dictionary/snippets (S22/S24). Commands for those answer
`unsupported_command` — checked *before* the capability gate, so a client is
told "this build cannot" rather than "you may not", which would send S32's UI
looking for a setting that does not exist.

Pipeline behavior is unchanged: same stages, same order, same fail-open
grammar pass. `dictate-core/src/agent.rs` was deleted rather than left beside
the new pipeline — a parallel legacy path is exactly the drift this slice's
"same code path" requirement exists to prevent.

## Notes for the next slice

- **S11 (VAD)** must flip `timings.vad` from `Skipped{not_supported}` to a real
  measurement — the honest placeholder is in `pipeline.rs::Stages::new`.
- **S12 (STT)** owns the final `SttProvider` shape; the minimal version in
  `ports.rs` exists only because the daemon is untestable without a seam.
  `ModelInfo::backend` currently reports the *configured* device; S12 must
  report the one actually selected at load time.
- **S20** should lift the rules pass out of `dictate-stt::transcribe` so
  `fmt_rules` can stop being `NotReported`.
- **S13** replaces `TextInjector`; note `is_available()` is called on the
  blocking pool before the commit point, deliberately, so a Wayland portal
  probe can block without stalling a worker or breaking cancellation.
- **S32/S33** get event fan-out and the ownership rule for free. A relay must
  forward raw bytes, not re-serialize parsed events (see the proto crate docs).

---

VERIFICATION_ITEMS:

### AUTOMATED

- `cargo test --workspace` — 379 passed, 0 failed (base 225 + 154 new).
  check: `just test` (or `WHISPER_DONT_GENERATE_BINDINGS=1 PATH="/opt/cuda/bin:$PATH" cargo test --workspace`)
  expected: `379 passed; 0 failed` across all suites; no suite reports FAILED.
- `cargo clippy --all-targets --workspace` — clean.
  check: `just clippy`
  expected: zero `warning:` or `error:` lines.
- Full pipeline over UDS with a mock STT provider, no CUDA and no model download.
  check: `cargo test -p dictated --test control_plane a_full_dictation_runs_the_protocols_state_machine_over_the_socket`
  expected: passes; asserts the walk `recording → transcribing → formatting → injecting → done`, a `final` event carrying the transcript, exactly one injection, then `done → idle`.
- Cancellation from every non-terminal state.
  check: `cargo test -p dictated --test control_plane cancel_from_`
  expected: 4 tests pass (`recording`, `transcribing`, `formatting`, `injecting` pre-commit); each asserts terminal state `cancelled`, zero injected text, and the capture stream released.
- Cancel after the injection commit point is refused, not falsely honored.
  check: `cargo test -p dictated --test control_plane cancel_after_the_commit_point_is_refused_rather_than_lying`
  expected: passes; the cancel returns `conflict`, the session ends `done`, and the injection completed intact.
- Commit point is mutually exclusive under real thread contention.
  check: `cargo test -p dictate-core cancel_and_commit_are_mutually_exclusive_under_contention`
  expected: passes 2,000 rounds; `cancel()==Accepted` and `enter_commit()==Some` never both hold.
- Concurrent-command safety: racing starts.
  check: `cargo test -p dictated --test control_plane racing_starts many_concurrent_starts`
  expected: both pass; exactly 1 session started, all losers get `busy` with `retry_after_ms: 500`.
- Concurrent-command safety: client disconnect mid-session.
  check: `cargo test -p dictated --test control_plane a_disconnect_`
  expected: both pass; an orphaned session is finishable by the next trusted client, and a disconnect mid-pipeline does not cancel it.
- Lost-wakeup regressions (stop/cancel immediately after start).
  check: `cargo test -p dictated --test control_plane immediately_after_start`
  expected: both pass, 25 iterations each; no hang.
- Session ownership: a second client cannot stop or cancel the first's session.
  check: `cargo test -p dictated --test control_plane a_second_client_cannot_cancel the_owner_can_stop`
  expected: both pass; the intruder gets `forbidden` on both `stop` and `cancel`; the owner succeeds.
- Session ownership policy, including the S33 remote-client property.
  check: `cargo test -p dictate-core session::tests`
  expected: 9 tests pass, including `a_remote_client_may_not_control_a_host_session`.
- Signal path produces identical state transitions to the protocol commands.
  check: `cargo test -p dictated --test control_plane sigusr1_produces_the_same_state_transitions_as_the_protocol_commands`
  expected: passes; the two transition sequences compare equal.
- Real kernel-delivered signals reach the engine.
  check: `cargo test -p dictated --test control_plane a_real_sigusr1_delivered_to_this_process_starts_a_session`
  expected: passes; a raised SIGUSR1 reaches `recording`, a raised SIGUSR2 reaches `cancelled`.
- Timings are honest per stage.
  check: `cargo test -p dictated --test control_plane per_stage_timings a_formatting_pass_that_burns_time`
  expected: both pass; `vad` is `skipped{not_supported}`, a rule-skipped `fmt_llm` is `skipped` (never `ran{0.0}`), `fmt_rules` is `not_reported`, a failed-open format pass is `failed{ms>=20}`, and `total_ms` is present and ≥ the stage sum.
- Runtime paths never collide with the Python daemon.
  check: `cargo test -p dictated paths::tests`
  expected: 12 tests pass, including `the_new_daemon_never_shares_a_path_with_the_python_daemon` and `the_legacy_file_is_left_alone_when_another_daemon_holds_it`.
- `dictate` CLI end to end against a protocol-speaking daemon.
  check: `cargo test -p dictate-cli`
  expected: 34 tests pass (23 unit + 11 binary-level), covering status rendering, toggle in both directions, exit codes 0/1/2, and the missing-daemon message.
- Python reference daemon untouched.
  check: `git diff 3fac46d..HEAD --stat -- dictate/ scripts/`
  expected: empty output — no changes to `dictate/` or `scripts/`.

### DAEMON

- Daemon starts, binds its socket, and writes only its own runtime files.
  check: `XDG_RUNTIME_DIR=/tmp/v/run XDG_CONFIG_HOME=/tmp/v/cfg XDG_DATA_HOME=/tmp/v/data ~/.local/bin/dictated & sleep 2; find /tmp/v -type s -o -type f`
  expected: `dictated.sock` and `dictated.pid` under `$XDG_RUNTIME_DIR/dictate-agent/`, plus the legacy `dictate.pid` claimed only because nothing else held it. No file under `~/.config/dictate-agent` or `~/.local/share/dictate-agent` is touched.
- `dictate status` reports state, daemon identity, and model backend.
  check: `dictate status`
  expected: `state idle`, `daemon dictated 0.2.0 (protocol v1)`, a pid and uptime, and a `model` line naming the backend (`cuda` or `cpu`) — the backend is load-bearing, since a CPU p50 is not comparable to a CUDA one.
- Jake's existing scripts still drive the daemon unchanged.
  check: with only `dictated` running: `scripts/dictate-toggle; sleep 1; dictate status; scripts/dictate-cancel; dictate status`
  expected: first `status` shows `state recording`; after cancel, `state idle`. Neither script was modified.
- Signals and socket agree on one session.
  check: `dictate tail &` then `scripts/dictate-toggle`, then `dictate cancel`
  expected: the tail shows `state idle -> recording` from the signal and `state recording -> cancelled` from the socket command — one session, one state machine, two surfaces.
- Clean shutdown releases everything.
  check: `systemctl --user stop dictated` (or SIGTERM), then `find $XDG_RUNTIME_DIR/dictate-agent`
  expected: socket and PID file removed; if a session was recording, it ends `cancelled` and the microphone is released.
- A second daemon refuses to start rather than stealing the socket.
  check: start `dictated` twice with the same `XDG_RUNTIME_DIR`
  expected: the second exits non-zero with `dictated is already running as pid <N>`; the first keeps serving.
- Python daemon and `dictated` coexist.
  check: start the Python daemon first, then `dictated`; run `scripts/dictate-toggle`
  expected: `dictated` logs that the legacy PID file is held by a live pid and leaves it alone; the script drives the Python daemon; `dictate status` still reaches `dictated` on its own socket.

### MANUAL

- Real dictation end to end on the RTX 5080 box: `scripts/dictate-toggle`, speak,
  toggle again, confirm text is pasted into the focused app and `dictate history`
  shows the session with a `total_ms` inside the ≤1.0s p50 budget. Requires a
  microphone, a display server, and the GGUF model — none available in CI.
- `dictate tail` while dictating, to eyeball that the HUD-facing event stream is
  legible and correctly ordered for S32.
