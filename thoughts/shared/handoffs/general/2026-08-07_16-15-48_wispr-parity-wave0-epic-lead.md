---
date: 2026-08-07T16:15:48-07:00
researcher: Claude (RSI Epic-lead session bba123fa)
git_commit: 943b556d566c3bf0096e0b30e45ef1ed1b9e656e
branch: rsi/bba123fa
repository: dictate_agent
topic: "Wispr Flow parity — Wave 0 execution (S00 + R1–R4 complete, S01 in flight)"
tags: [orchestration, epic-lead, wave0, wispr-flow, rust, workspace, research-spikes]
status: in_progress
last_updated: 2026-08-07
last_updated_by: Claude (RSI Epic-lead session bba123fa)
type: orchestration_handoff
---

# Wispr Flow Parity — Wave 0 Epic-Lead Handoff

## Immediate Next Action

Review S02's daemon control plane when child session `c0ce10fb` completes
(dispatched and running at handoff time). Check especially: cancellation from
every non-terminal state, session-ownership enforcement on Stop/Cancel, the
concurrent-command tests, and that the signal path genuinely shares the
command code path rather than duplicating it. Then open the **Wave 1 fan-out**
— S10, S11, S12, S13, S30, S31 can all go in parallel per the DAG.

## Task(s)

- Execute the Epic-lead kickoff prompt in the master slice map — **in progress**
- Wave 0 research spikes R1–R4 — **complete** (all four returned verdicts)
- S00 repo reconciliation & workspace scaffold — **complete, verified (78 tests)**
- S01 protocol crate — **complete, reviewed, APPROVED with one overrule (225 tests)**
- S01-FIX deny-by-default route gating — **complete, verified (225 tests)**
- S02 daemon skeleton — **dispatched, in flight** (`c0ce10fb`)
- Wave 1 fan-out — **not started** (gated on S02)

## S01 review — approved, with one security overrule

S01 delivered `dictate-proto` (225 workspace tests green, clippy clean,
`docs/protocol.md` for S32/S33 implementers) and — good behavior worth
repeating — flagged five of its own decisions for the lead to overrule rather
than burying them. Four were accepted; one was a real defect.

**Overruled: `Capabilities::allows_route` was fail-open.** An empty `routes`
list read as "allow everything", and *both* `Capabilities::default()` and any
JSON omitting the field produce exactly that state — while `Features` sitting
beside it in the same struct is deliberately deny-by-default. The exposed path
is not theoretical: `Route::Timer` runs `systemd-run` on the host and S33 will
serve this protocol over the LAN, so a config bug or a hand-rolled client
could have obtained host command execution. Fixed in `5f15051`
(`routes.contains(route)`), wire format unchanged, 23/23 golden tests
unaffected, verified by the lead.

Accepted as-is: the added `Event::InjectionResolved` (a non-terminal
`AwaitingConsent` needs one, and adding it post-S33 would be breaking); no
session id on Stop/Cancel (S02 enforces ownership daemon-side; an optional
field is additive later); `raw_text` gated only by docs (deferred to S33's
security review — a capability flag is additive); `docs/protocol.md` living
outside `thoughts/` (it is implementer documentation, not a thoughts artifact).

Notable design choices now locked: additive-only compatibility within
`PROTOCOL_VERSION`, deliberately asymmetric on unknowns (unknown event/result
degrades to `Unknown`; unknown *command* fails so the server answers
`unsupported_command` rather than stranding a caller); four-state per-stage
timings (`Ran`/`Skipped`/`Failed`/`NotReported`) so a skip-rule skip stays
distinguishable from a 0ms run — which is what makes S12's latency obligation
and the ≤1.0s budget measurable rather than aspirational.

## Findings that changed the plan

Three of the slice map's stated premises were false on inspection. All three
are now corrected in the map itself (commit `083ac86`, merged to master):

1. **The Python reference daemon had been deleted.** Commit `5e16667`
   ("saving agent created work") removed all 13 modules of `dictate/`
   (2,304 LOC) plus `CLAUDE.md` as collateral inside an untargeted commit,
   while legitimately advancing the Rust code. This silently violated locked
   decision #8 (side-by-side migration) and the v1.0 cutover gate, both of
   which require the Python daemon to stay runnable. Nothing was lost —
   `dictate/` was intact on `origin/master`, and local `master` is 5 commits
   ahead / 0 behind, all unpushed. Folded into S00 as a first standalone
   restore commit (`b458657`), restored verbatim, verified present.
2. **The test baseline was 78, not 74.** Verified green (exit 0) before
   dispatching anything, using `WHISPER_DONT_GENERATE_BINDINGS=1` and
   `PATH="/opt/cuda/bin:$PATH"`. 78 is now the floor S00 was held to.
3. **The `src_rust_archive/` discrepancy was moot.** No such directory
   exists; `src/` was the single source of truth; the stale `CLAUDE.md` that
   claimed otherwise had itself been deleted by `5e16667`.

## Wave 0 results

**S00 — workspace scaffold · COMPLETE · independently verified by the lead.**
`cargo test --workspace` = 78 passing / 0 failed (lead re-ran it, did not take
the worker's word), clippy clean, release build OK, Python restored (13 files),
`src/` fully migrated. Seven crates created — dictate-audio, dictate-stt,
dictate-fmt, dictate-history, dictate-inject, dictate-core, dictated — and
**no empty placeholder crates**, which was an explicit scope tightening over
the original S00 brief (placeholders for slices that do not own code yet would
have created merge churn and false structure).
S00 surfaced one genuine design constraint rather than papering over it:
`config.rs` could not move wholesale into `dictate-core`, because dictate-core
already depends on every leaf crate, so a leaf depending back for its own
Config sub-struct would be circular. Resolved by each leaf owning its domain
Config, re-exported into the aggregate. The ~8-line `expand_tilde` helper is
duplicated in two crates for the same reason. Both documented in the commit.

**R1 — Handy cannibalization · COMPLETE.** 25 findings (12 ADOPT / 6 ADAPT /
7 AVOID) against a pinned clone (`b428ae4c`). No AVOID invalidates a locked
decision. Handy is MIT — verbatim reuse needs an attribution NOTICE; patterns
do not. Skipped (time-boxed): Whispering + VoiceInk secondary passes.

**R2 — Streaming partials · COMPLETE · verdict DEFER.** whisper-rs exposes no
state-reuse/incremental-decode API, so every partial tick is a full
mel→encoder→decoder pass contending with the authoritative decode inside the
same ≤1.0s budget. Revisit trigger: after S12 produces real turbo-CUDA
numbers. S12's brief now carries that measurement as an explicit obligation.

**R3 — Wayland · COMPLETE.** GNOME's baseline for both injection and window
context is *absent or consent-gated*, not merely degraded. Clipboard
save/restore is unsupported on GNOME Wayland, which limits locked decision #5
there (X11, Jake's actual environment, is unaffected). Produced two hard trait
constraints now recorded in S13/S23.

**R4 — Wake word · COMPLETE · verdict BUILD-S34, conditional.** openWakeWord
via `oww_rs` (Apache-2.0) is the pick. **Porcupine dropped** — it phones home
to validate a license key, disqualifying for a fully-local product, and its
free tier ended 2026-06-30. No engine has a measured idle-CPU figure, so S34
must open with a benchmark and defer itself if idle cost exceeds ~2–3% of a core.

## Research propagated into consuming slices

Research that sits unread in a file is wasted. Each spike's actionable output
was written inline into the slices that consume it, so those workers get it
without reopening the audits: S31 (hotkey release-grace debounce for X11
auto-repeat; static cancel chord), S32 (overlay architecture; **click-through
is NOT solved in Handy** — open design work), S12 (sha256 verify-then-delete,
hf-hub revision pinning, default-pull on first run, + the R2 measurement
obligation), S13 (async `Injector::inject()` with a "pending consent" outcome;
capability-probed paste-vs-type), S23 (`ContextProvider` must treat "no
context" as a first-class value, not an error), S42 (`$ORIGIN` rpath, CI
fallback-verification staging, CUDA-for-Vulkan substitution), S34 (engine
choice + the idle-CPU gate).

## Harness observation — worker commit targets are inconsistent

Workers did not consistently use their own sandbox branches. R1 and R3
committed to their sandbox branches (`rsi/76789b78`, `rsi/f24927f5`); **R2, R4,
and S00 committed directly to `master` in Jake's main checkout**
(`/home/jakedevar/dictate_agent`). No work was lost and no collision occurred —
paths were disjoint and staging was narrow — but this defeats sandbox
isolation and is the same failure mode that produced the destructive `5e16667`.

Mitigations applied: sent S00 a mid-run advisory forbidding `git add -A` while
R3/R4 were concurrently writing into the same tree; gave S01 an explicit
base-state self-check (stop and report if `crates/` is missing) so a stale
branch base can never silently recreate prior work. S01 is confirmed working
correctly in its own sandbox at base `49d2332`.

Also note: because workers read `master` from the main checkout, Epic-lead
slice-map updates had to be merged to `master` (`49d2332`) or downstream
workers would have inherited stale guidance. Successor leads must keep doing
this or their planning updates will not reach their workers.

## Critical References

- `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md` (updated)
- `thoughts/shared/handoffs/general/2026-08-07_22-49-51_s00-repo-reconciliation-workspace-scaffold.md`
- `thoughts/shared/research/2026-08-07-r1-handy-cannibalization-audit.md` (branch `rsi/76789b78`)
- `thoughts/shared/research/2026-08-07-r2-streaming-partials-feasibility.md`
- `thoughts/shared/research/2026-08-07-r3-wayland-injection-context.md` (branch `rsi/f24927f5`)
- `thoughts/shared/research/2026-08-07-r4-wake-word-bakeoff.md`

## Action Items & Next Steps

- **Lead:** review S01 `dictate-proto` output (`c25bc78e`) — approve or iterate
  before S02. Check especially: versioned envelope + capability handshake,
  timings expressiveness (skipped-stage vs 0ms), the consent-gated outcome
  enum, and golden-JSON pinning.
- **Lead:** dispatch S02 (Feature, opus/xhigh) after S01 approval.
- **Lead:** open Wave 1 fan-out (S10, S11, S12, S13, S30, S31) per the DAG.
- **Lead:** merge R1's and R3's sandbox branches into master, or their audits
  stay invisible to workers reading master.
- **Jake:** Q6–Q10 remain open with defaults in force (history default,
  Windows deferral, TIMER/LOCAL preservation, model disk budget, Python
  retirement gate).
- **Jake:** `master` holds 5+ unpushed commits including the destructive
  `5e16667`. Worth a look before any push.

## Other Notes

- Nothing pushed, per policy. All commits local.
- Python daemon restored and must stay runnable until the v1.0 parity gate.
- Wave 0 spend: ~$7.30 across five workers (R1 $2.33, R2 $1.04, R3 $1.77,
  R4 $2.15, S00 not yet totalled at handoff time).
