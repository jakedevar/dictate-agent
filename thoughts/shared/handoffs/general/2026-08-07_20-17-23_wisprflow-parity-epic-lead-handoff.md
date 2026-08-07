---
date: 2026-08-07T13:17:23-07:00
researcher: Claude (RSI master orchestration session)
git_commit: a7897ec0395f3389a264a072657e8b136559472e
branch: rsi/bb349467
repository: dictate_agent
topic: "Wispr Flow Parity Build Implementation Strategy"
tags: [implementation, strategy, orchestration, slice-map, wispr-flow, rust]
status: complete
last_updated: 2026-08-07
last_updated_by: Claude (RSI master orchestration session)
type: implementation_strategy
---

# Wispr Flow Parity Epic — Orchestration Handoff

## Immediate Next Action

Dispatch S00 worker (sonnet/high) via rsi-rpc AgentSpawnChild; fan out R1–R4 research spikes in parallel.

## Original Request

Rebuild dictate-agent to functional parity with Wispr Flow (backend over UI), nearly everything in Rust, Tauri/TypeScript UI acceptable, Arch Linux first and macOS second, stretch goal of a LAN-hosted dictation API server reachable from other machines. This session produced the master orchestration slice map baseline; the successor Epic-lead executes it.

## Task(s)

- Author master slice map (parity matrix, architecture, 25 slices, DAG, dispatch table) — done
- Record Jake's Q1–Q5 decisions; S33 phone-client protocol requirement — done
- Write Epic-lead kickoff prompt into slice map final section — done
- Wave 0 dispatch (S00 → S01 → S02) plus R1–R4 spikes — planned
- Waves 1–4 execution per slice map DAG — planned

## Critical References

- thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md
- thoughts/shared/handoffs/general/2026-04-10_17-51-32_rust-rewrite-implementation.md
- thoughts/shared/research/2026-04-10-rust-go-rewrite-feasibility.md

## Recent Changes

- thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md:1 — new master slice map (commit 2786964)
- thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md:330 — decisions, phone-client note, kickoff prompt (commit a7897ec)

## Learnings

- Rust port at src/ is code-complete vs Python daemon (74 tests pass); CLAUDE.md's src_rust_archive/ claim is stale on this branch — S00 reconciles against master.
- whisper-rs CUDA build requires WHISPER_DONT_GENERATE_BINDINGS=1 and /opt/cuda/bin on PATH — see 2026-04-10 handoff Learnings.
- Measured baseline: 183ms avg transcription (HF, RTX 5080), 0.87s avg pipeline; whisper.cpp projected 400–600ms, so the ≤1s parity budget holds.
- Wispr's 2026 surface adds command mode, snippets, wake word, scratchpad, per-app styles — all mapped in the parity matrix.
- Handy (MIT, Rust+Tauri+whisper-rs, three OSes) is the prime R1 cannibalization target.
- rsi-rpc agent verbs verified live in this sandbox; workers spawn via AgentSpawnChild.

## Artifacts

- thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md
- thoughts/shared/handoffs/general/2026-08-07_20-17-23_wisprflow-parity-epic-lead-handoff.md

## Action Items & Next Steps

- Jake: spawn Epic-lead session (opus, xhigh) using the kickoff prompt in slice map final section.
- Lead: dispatch S00 (Refactor, sonnet/high); require all 74 tests green after workspace split.
- Lead: fan out R1–R4 research spikes in parallel with S00.
- Lead: dispatch S01 (opus/xhigh); personally review protocol design before S02 consumes it.
- Lead: dispatch S02, then open Wave 1 fan-out (S10–S13, S30, S31) per DAG.
- Lead: enforce worker contract — thoughts commits, verification buckets, never git push.
- Lead: escalate at S01 approval, wave completions, parity gates, LAN-security decisions, Q6–Q10 forcings.

## Other Notes

- Python daemon stays live until v1.0 parity gate; new daemon uses distinct socket/PID/DB paths.
- All commits local to rsi/bb349467; nothing pushed, per policy.
