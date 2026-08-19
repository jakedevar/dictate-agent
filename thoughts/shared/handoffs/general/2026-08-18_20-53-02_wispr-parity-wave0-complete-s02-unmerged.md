---
date: 2026-08-18T20:53:02-07:00
researcher: Claude (RSI Epic-lead session bba123fa)
git_commit: 73b2670
branch: rsi/bba123fa
repository: dictate_agent
topic: "Wispr Flow parity — Wave 0 COMPLETE; S02 verified but unmerged; Wave 1 ready to dispatch"
tags: [orchestration, epic-lead, wave0, wave1, wispr-flow, rust, handoff]
status: complete
last_updated: 2026-08-18
last_updated_by: Claude (RSI Epic-lead session bba123fa)
type: orchestration_handoff
---

# Wispr Flow Parity — Wave 0 Complete, Handoff to Successor Epic-Lead

## Immediate Next Action

**Merge S02 (`rsi/c0ce10fb`, 5 commits, HEAD `8d15b28`) into `master`.** It is
complete and I have independently verified it — 379 tests passing / 0 failed,
clippy clean. It has been sitting unmerged for 11 days because the review wake
fired while S02 was still running, the rescheduled wake was interrupted by an
rsid daemon upgrade, and Jake then paused the epic.

Merging it also fixes a live documentation defect on master (see "Known defect
on master" below). After merging, dispatch the **Wave 1 fan-out**.

## Where the epic stands

Wave 0 is **done**. Nothing is in flight. No child sessions are running.

| Slice | Status | Evidence |
|---|---|---|
| S00 workspace scaffold | ✅ merged | 78 tests, 7 crates, Python restored |
| R1 Handy audit | ✅ merged | 12 ADOPT / 6 ADAPT / 7 AVOID |
| R2 streaming partials | ✅ merged | verdict **DEFER** |
| R3 Wayland | ✅ merged | two hard trait constraints |
| R4 wake word | ✅ merged | verdict **BUILD-S34**, conditional |
| S01 `dictate-proto` | ✅ merged | 225 tests; approved w/ 1 overrule |
| S01-FIX route gating | ✅ merged | `5f15051` deny-by-default |
| **S02 control plane** | ⚠️ **COMPLETE, UNMERGED** | **379 tests, clippy clean (lead-verified 2026-08-18)** |
| Wave 1 (S10–S13, S30, S31) | ⬜ not dispatched | gated only on the S02 merge |

`master` HEAD is `3fac46d`, unchanged since 2026-08-07. Epic spend to date:
**~$49** across 8 worker sessions.

## S02 — what it delivered (verified, not just claimed)

I re-ran the suite on `rsi/c0ce10fb` myself on 2026-08-18: **379 passed, 0
failed** (base 225, +154), `cargo clippy --all-targets --workspace` clean, and
the `tests/control_plane.rs` integration binary genuinely runs. S02's own
report was accurate in every respect I checked.

Design highlights worth knowing before reviewing Wave 1 work that builds on it:
- **Cancellation**: one task owns a session from `Recording` to terminal via a
  single `finish()` teardown; long awaits are raced against a cancel token.
  Injection sits behind an atomic **commit point** — `enter_commit()` and
  `cancel()` are the two exits from one compare-exchange — so a late cancel
  returns `conflict` rather than falsely claiming nothing was typed.
- **Session ownership**: enforced daemon-side, keyed on the `host_capture`
  capability rather than the transport. A trusted-local connection may control
  an unowned host session (this is what lets one `dictate` invocation start and
  the next one stop); signals outrank both; a peer without `host_capture` can
  never touch another's session. Disconnect **orphans** rather than cancels,
  otherwise the CLI could not work.
- **Concurrency**: serialized by construction — every command from every
  connection *and* from the signal handler flows through one mpsc into one
  engine task. Racing starts yield exactly one session; losers get `busy`.
- **Signal path shares the command path** (verified by S02 with real kernel
  signals): `signals.rs` holds no recording logic; SIGUSR1 calls
  `EngineHandle::toggle` into the same handlers the protocol uses.
- **Python non-collision** confirmed: distinct socket/PID/DB; `dictate/` and
  `scripts/` byte-identical by diff; legacy PID file taken claim-if-free.
- Two real bugs were caught by its own tests: a lost wakeup where
  `notify_waiters` dropped a `Stop` arriving before the session task parked
  (would hang a push-to-talk tap forever), and `kill(pid, 0)` treating PID 0 as
  the caller's process group (a zeroed PID file would have locked the daemon
  out permanently).

## Known defect on master (fixed by merging S02)

`docs/protocol.md:126` on master still states that `routes` is an "**empty list
means unspecified** and is read permissively". That is **wrong and dangerous**:
the code has been deny-by-default since `5f15051`. S02 corrected the doc on its
branch. Until the merge lands, any S32/S33 implementer reading master's doc
gets fail-open guidance for the capability that gates `Route::Timer`
(`systemd-run` on the host). Merge S02, or fix the doc immediately.

## Two protocol gaps S02 raised (Epic-lead decisions, not taken unilaterally)

1. **No `toggle` command in the protocol.** The CLI must therefore
   read-then-act, giving `dictate toggle` a TOCTOU window that the signal path
   does not have. S02 recommends adding it. **My recommendation: approve.** It
   is purely additive under the protocol's stated compatibility rule, so it
   costs nothing now and closes a real race. Dispatch as a small `S02-FIX`
   (Bug, sonnet/high) or fold into the first Wave 1 slice that touches the CLI.
2. **`raw_text` is gated only by documentation.** Deferred earlier to S33's
   security review; a dedicated capability flag is additive, so deferring
   remains safe. Do not let S33 ship LAN exposure without revisiting it.

## Traps — read before touching git

- **Do NOT merge `rsi/bb349467`.** It is 2 commits "ahead" of master but is a
  stale pre-epic branch; merging it deletes ~10,000 lines including the R3 and
  R4 research artifacts. Leave it alone.
- **Workers commit inconsistently.** R1 and R3 used their sandbox branches;
  R2, R4, and S00 committed directly to `master` in Jake's main checkout
  (`/home/jakedevar/dictate_agent`). No work was lost, but this defeats sandbox
  isolation and is the same failure mode that produced the destructive
  `5e16667`. Give every worker an explicit base-state self-check.
- **Epic-lead planning edits must be merged to `master`.** Workers read the
  slice map from the main checkout. Updates that live only on the lead's
  sandbox branch are invisible to them.
- **`master` is 31 commits ahead of `origin/master`; nothing is pushed**, per
  policy. That history includes `5e16667`, the commit that deleted the Python
  daemon. Worth Jake's review before any push.
- **Build env vars are mandatory**: `WHISPER_DONT_GENERATE_BINDINGS=1` and
  `PATH="/opt/cuda/bin:$PATH"` (S00 encoded these in a `justfile`/`Makefile`).
- **Test baseline after merging S02 is 379.** Hold Wave 1 workers to it.

## Wave 1 — ready to dispatch (all parallel, per the DAG)

S10 audio v2, S11 VAD, S12 STT v2, S13 injection v2, S30 history v2, S31
hotkey service. All are Feature/Task, **sonnet/high**. The slice map already
carries per-slice research constraints propagated from R1–R4 — brief each
worker to read its own slice entry, since that is where those live.

Two obligations not to lose:
- **S12 must record real turbo-CUDA p50/p95 decode latency.** That measurement
  is the gate that converts R2's streaming-partials DEFER into a GO/NO-GO, and
  it is the only evidence that the ≤1.0s budget holds.
- **S13/S23 trait shapes are constrained by R3**: `Injector::inject()` async
  with a "pending consent" outcome; `ContextProvider` treating "no context" as
  a first-class value, not an error.

## Critical References

- `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md` — the
  plan; slices carry inline R1–R4 findings and the S01 decision record
- `thoughts/shared/handoffs/general/2026-08-07_16-15-48_wispr-parity-wave0-epic-lead.md` — prior lead handoff
- S02's own handoff on `rsi/c0ce10fb`:
  `thoughts/shared/handoffs/general/.../s02-*.md` (commit `8d15b28`)
- `docs/protocol.md` + `crates/dictate-proto/tests/golden.rs` (the golden tests
  are authoritative over the prose)
- Q6–Q10 in the slice map remain open with defaults in force; none blocked Wave 0.

## Successor kickoff prompt (paste verbatim)

Recommended spawn: **opus @ xhigh effort**, lead permissions.

> You are the Epic-lead orchestration agent for the dictate-agent → Wispr Flow
> parity build, running with lead permissions inside the RSI harness. The plan
> already exists — do not re-derive it, and do not re-run Wave 0.
>
> Sources of truth, in order: (1) `thoughts/shared/handoffs/general/2026-08-18_20-53-02_wispr-parity-wave0-complete-s02-unmerged.md`
> — current state, verified facts, traps, and the Wave 1 brief; (2)
> `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md` —
> architecture, slices, DAG, dispatch table, locked decisions, parity gate
> (each slice entry carries the R1–R4 research findings already propagated
> into it); (3) `thoughts/shared/handoffs/general/2026-04-10_17-51-32_rust-rewrite-implementation.md`
> — whisper-rs/CUDA build traps.
>
> **Wave 0 is complete.** S00, S01 (+ its security fix), S02, and research
> spikes R1–R4 are all done. Nothing is in flight.
>
> Your first action: **merge S02 (`rsi/c0ce10fb`, 5 commits, HEAD `8d15b28`)
> into `master`.** It is complete and was independently verified on 2026-08-18
> — 379 tests passing / 0 failed, clippy clean — it simply never got merged.
> Merging also repairs a live defect: master's `docs/protocol.md:126` still
> tells implementers that an empty `routes` list is read permissively, which
> contradicts the deny-by-default code (`5f15051`) and is fail-open guidance
> for the capability gating `Route::Timer` (`systemd-run` on the host).
> Re-verify after merging rather than trusting this prompt.
>
> Then decide S02's one open protocol question: it recommends adding a
> `toggle` command, because without it the CLI must read-then-act and
> `dictate toggle` carries a TOCTOU window the signal path does not. The prior
> lead recommends approving — it is purely additive under the protocol's
> compatibility rule. Dispatch it as a small `S02-FIX` (Bug, sonnet/high) or
> fold it into a Wave 1 slice that touches the CLI.
>
> Then open the **Wave 1 fan-out** — S10, S11, S12, S13, S30, S31, all in
> parallel per the DAG, Feature/Task at **sonnet/high**. Brief each worker to
> read its own slice entry, since the R1–R4 constraints live there. Two
> obligations must not be dropped: **S12 must record real turbo-CUDA p50/p95
> decode latency** (it is the gate that converts R2's streaming DEFER into a
> GO/NO-GO and the only evidence the ≤1.0s budget holds), and **S13's
> `Injector` trait must be async with a "pending consent" outcome** per R3.
>
> Your job is dispatch and supervision, not implementation. Spawn one child
> worker per slice via `rsi-rpc AgentSpawnChild` (fields: `kind`, `model`,
> `effort`, `query`, `tags`, `idempotency_key`; note `rsi-rpc` prints a banner
> line before its JSON, so strip it before piping to `jq`). Give every worker
> an explicit **base-state self-check** — workers in this harness have
> inconsistently committed to `master` in the main checkout instead of their
> sandbox branch, so a stale base must fail loudly rather than silently
> recreate finished work. **Verify every worker's claims by re-running tests
> yourself; do not merge on a worker's say-so.** Merge your own slice-map
> updates into `master`, or workers will read stale guidance.
>
> Constraints (HARD): NEVER `git push` — Jake pushes after his own gates.
> Never merge `rsi/bb349467` (stale pre-epic branch; merging it deletes ~10k
> lines including the R3/R4 research). Keep the Python daemon (`dictate/`)
> untouched and runnable until the v1.0 cutover gate; the new daemon already
> uses distinct socket/PID/DB paths. Build env: `WHISPER_DONT_GENERATE_BINDINGS=1`
> and `PATH="/opt/cuda/bin:$PATH"`. Test floor after the S02 merge is **379**.
> Escalate to Jake at: wave completions, Q6–Q10 decisions when a slice forces
> one, any LAN-exposure/security decision (S33), and the v0.9/v1.0 parity gates.
