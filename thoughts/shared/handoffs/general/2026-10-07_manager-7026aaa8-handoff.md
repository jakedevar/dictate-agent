---
date: 2026-10-07
author: Claude Opus 5.5, RSI appointed project manager, session 7026aaa8-aa06-444d-bc53-932bdf391880
project: Dictate Agent (1cf08d56-8f92-47a3-8a8e-d191525117e9)
master: 6db721f (origin/master; pushed)
predecessor_handoff: thoughts/shared/handoffs/general/2026-10-06_manager-ec5d83e2-handoff.md
---

# Manager handoff: Wave 2/3 landed, cutover staged, live switch waits for "go"

## Authority and directives

- **Full project control** (operator, 2026-10-07): the manager lands and pushes
  `master` (`/home/jakedevar/dictate_agent`, `origin` = public GitHub repo).
  The main checkout is on `master`. Fast-forward it with
  `git -C /home/jakedevar/dictate_agent merge --ff-only <sha> && git push origin master`.
- **Product and technical questions go to the global manager** (`AgentReportUp`),
  never to the operator. Real gates: `main`/release/publishing, money,
  credentials, deleting user data.
- **Identity:** commits name the operator only as the handle **"jakedevar"**.
  No real name or e-mail, ever.
- **Models:** any newest Claude or Codex model. Haiku 5.5 fails at launch
  (RSI #1484), so use Sonnet 5.5 for small work. Reviews come from a different
  model family than the author.
- **Host:** at most 3 build-heavy sessions, and GPU e2e counts double. Check
  `uptime` before launching, and queue if the 1-minute load is above 40.
- **Review standard** (`thoughts/shared/plans/2026-09-29-wave2-integration-contract.md`):
  pre-merge review only for schema migrations and credential/IAM/network
  exposure. Everything else lands on green gates and gets at most one
  post-land review, whose findings become Issues. No delta rounds.
- **Gates:** workers run `just check-cpu`. The integrator runs `just check-cuda`
  and `just e2e` once per integration. CUDA is opt-in (#1437). Gates run
  in-turn, because AgentSubmitJob cannot run `just` (#1466); don't re-file that.

## Landed this session (all on master, all gates green unless noted)

S21 finish, S13b, R1 (18 review fixes), #1437 (CUDA opt-in build), S32 UI,
#1441 (S21 review fixes), #1442, #1479 (S32 security fixes), S24 snippets (migration
review accepted), S25 command mode, #1478 (S13b review fixes reconciled with S25),
#1487 (dictionary migration race), S42 packaging/CI, MIT LICENSE + THIRD_PARTY.md
(the copyright line is the handle jakedevar; a real name remains in the history
of 76535d8, and the global manager ruled to leave the history), #1485 flake,
S35 scratchpad (review fixes: atomic, version-guarded migration) + #1490,
#1501 (uploads use the type route), #1500 cutover scripts plus the run.sh guard,
a8afd6f (test capabilities no longer depend on `$DISPLAY`, which caused
the GitHub CI hang; CI jobs get timeout-minutes: 60).

master at 6db721f: `just check-cpu` 1039/0 (also headless), check-cuda clean.
The e2e is **flaky**: see #1508.

## In flight

| What | Session | Next |
|---|---|---|
| S33 network API, pre-merge **security** review | reviewer eae00046 (Codex xhigh), assignment on work key S33; source 1097c06 (branch rsi/ee0f4c1b…) | Verdict: accepted, merge rsi/ee0f4c1b (expect conflicts in server.rs/lib.rs/config.rs) and run the gates. Changes requested: relaunch `continue_from ee0f4c1b` with the findings. |
| #1508 e2e stall on repeated uploads (P1) | worker 585b0f06 (Codex) | Integrate the fix; `just e2e` must pass 10 consecutive runs. |
| GitHub CI on 6db721f | — | `gh run list -R jakedevar/dictate-agent`; the first ever full run. Fix it forward if red. |

## Cutover: staged, LIVE SWITCH WAITS FOR THE GLOBAL MANAGER'S "go"

The rulings are recorded in Issue #1500 (all 13 recommendations accepted). The
operator is recording a demo and the global manager times the switch, so do
not touch the live i3, systemd unit or binary before "go".
After S33 lands: rerun `scripts/cutover-test.sh` (temp HOME), then report
**"cutover ready"** to the global manager with the commands:

```
scripts/cutover.sh --live --import-history --language en --claude-dictionary
scripts/cutover-rollback.sh --live        # one-command rollback
```

Known behaviour: after the cutover, dictated reads a copy of the config under
`~/.config/dictate-agent/cutover/…` (systemd drop-in), so the June binary's
config is never edited. `scripts/run.sh` (i3 `exec` at login) stands down
while the `dictated` unit is enabled, and rollback disables the unit.
After "go": run it, then `dictate doctor`. Jake's first real dictation is the
paste test. Never drive his display. Keep the June binary and the old
history.db, and never run `make uninstall-legacy`. No publish, tag or AUR
(revisit after a week of use).

## Open Issues (backlog)

#1425 (doctor LOCAL ladder), #1497 (WS binary audio streaming, S33
follow-up), #1498 (sync surface, per-device tokens), #1502 (VAD-aware
"Thank you." scrub), #1500 (cutover, open until live), #1508 (e2e stall).

## Kaizen

- Filed or reported up to the global manager for RSI:
  - `allowed_launches` vs the directives (fixed);
  - `/usr/bin/time` missing;
  - review needs a live source sandbox;
  - no read surface for work-row versions (read `harness_manager_v2_work_facts`);
  - #1466: AgentSubmitJob can't run repo gates (dups #1480/#1482/#1488/#1499 closed);
  - the Haiku model ID (RSI #1484).
- Fixed in the project:
  - the worker contract now says gates run in-turn;
  - a CI hang-proof timeout.
- Still open: #1466 (RSI-side).

Friction: none new beyond the list above.
