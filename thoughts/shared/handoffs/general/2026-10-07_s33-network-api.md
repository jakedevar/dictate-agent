---
date: 2026-10-07
author: Claude Opus 5.5 (S33 worker ee0f4c1b, Issue #1470)
slice: S33 network API server (dictate-server)
base_commit: 783b849
design: thoughts/shared/plans/2026-10-07-s33-network-api-security-design.md
status: green; needs ONE pre-merge security review (other model family) before merge
---

# S33 handoff: network API server

## What landed (branch rsi/ee0f4c1b-…, on top of 783b849)

| Commit | What |
|---|---|
| `docs(s33)` design | threat model, auth, TLS vs plaintext, per-command exposure, `raw_text`, limits, logging |
| `feat(proto)` | additive `Features::raw_text` (local true, remote false), golden pin, docs |
| `feat(server)` | new crate `dictate-server`: axum HTTP/1.1 + WS, bind policy, token store, Host/Origin gate, throttle and lockout, limits, rustls (ring) |
| `feat(config)` | `[api]` aggregated in `dictate-core::config`, validation, example config |
| `feat(dictated)` | `NetworkConnection` over the socket's own `process`/`dispatch`, `network.rs` grant and `Backend`, `Daemon::start_network_api`, `--api-token` and `--rotate-api-token`, the shutdown lost-wakeup fix |
| `docs(s33)` reference | `docs/protocol.md` S33 section, CLAUDE.md layout, as-built design notes |

## Threat model, in short

- **LAN attacker:** gets nothing. There is no LAN bind without `allow_lan`, the
  token, and TLS or the tunnel acknowledgment. Every-interface binds are
  refused. Even a token holder gets only the transcription grant.
- **Local process:** a same-UID process already has the stronger unix socket.
  A unit test checks that the network grant is a strict subset of the socket
  grant. Another UID needs the 0600 token file.
- **Browser CSRF and DNS rebinding:** blocked four ways. There is no ambient
  credential, any `Origin` is refused, `Host` must be on the allow-list, and
  the token is required.

## Numbers (CPU gate, `just check-cpu`, this branch)

- Tests: 1006 passed, 0 failed, 0 ignored. Clippy is clean on all three lines,
  fmt is clean, and the `dictate-cli` tree check is clean.
- New tests: `dictate-server` has 39 unit tests and 19 transport tests;
  `dictated` has 6 unit tests, 11 `network_api` tests and 1 `network_logs`
  test; `dictate-core` has 5 `api_tests`; `dictate-proto` has 1. All bind
  `127.0.0.1:0`.
- No CUDA, no `just e2e`: the diff does not touch `dictate-stt`, the
  transcription path or the CUDA config.

## Deviations (also in design §11)

1. No binary audio streaming over WS yet (engine changes belong to #1478's
   files). Binary frames close the session with 1003, and
   `begin_audio_stream`/`end_audio_stream` stay `unsupported_command`.
2. The token is required on loopback too.
3. The token is never minted at startup; `dictated --api-token` creates it.
4. `0.0.0.0` and `::` binds are refused.
5. No dictionary or snippet sync surface; that needs scoped per-device tokens.

Follow-ups filed: #1497 (engine-fed WS audio streaming), #1498 (per-device
scoped tokens and the sync scope). Kaizen: #1499 (the worker contract and the
RSI catalog disagree on how to run long builds).

## Conflict risks

- `crates/dictated/src/server.rs`: additive. There are new `Conn` fields,
  `visible()` replaces `wants()` at the one event-write site, `transcribe_audio`
  takes `&mut Conn`, and `NetworkConnection` is new. Neither S25 nor #1478 owns
  this file, but its dispatch arms are shared.
- `crates/dictated/src/lib.rs`: `Daemon` has two new private fields,
  `shutdown(mut self)`, and `notify_one` instead of `notify_waiters`.
- `crates/dictate-core/src/config.rs`: a `Config.api` field, an extra line each
  in `finalize` and `validate`, and a new `api_tests` module.
- Shared files, appended in their own blocks: `Cargo.toml`, `Cargo.lock`,
  `config/config.example.toml`, `docs/protocol.md`, `CLAUDE.md`.

## Pre-merge security review focus

See design §13: bind plan and startup wiring; token store; middleware order;
`NetworkConnection` (grant, owner, event scoping, redaction, scrub, drop leading
to cancel); the ceiling and the subset test; resource bounds; TLS; logging.

## Review 7a0c4c63 fix round (worker a05de99b, 2026-10-07)

The one pre-merge security review (Codex gpt-6.1-sol xhigh, receipt
7a0c4c63-0c56-42da-914a-17cd010603c5) requested changes. This round, on branch
`rsi/a05de99b-…`, continues from 1097c06:

1. `fbc81f5` merges master 743400d (S24 snippets, S25 edit, S35 notes, #1479,
   #1478, #1508). Text conflicts were in `config/config.example.toml` (`[edit]`
   and `[api]` both kept) and `docs/protocol.md` (the `raw_text` row kept; "all
   six" routes). server.rs, lib.rs and config.rs auto-merged.
   - #1508's cancel-safe `read_line_bounded`/`ConnReader` is untouched. The
     network path never reads a byte stream: a WS message arrives whole, and
     the reader task is its own (cancel-safe `StreamExt::next`).
   - Semantic fallout: snippets are now implemented, so over the network they
     are `forbidden` (not `unsupported_command`). The network tests pin
     snippets, notes and the `edit`/`note` routes as forbidden.
2. `1497d53` fix(core): NETWORK_TRANSCRIPTS_LOGGED, per the manager ruling.
3. `80e7e9f` fix(server): the other seven findings. Design §15 has the
   finding → fix → test table.
4. `ee86d22` fix(server): a WS request is also abandoned when its peer
   disconnects, which completes WS_IDLE_SHUTDOWN_NOT_ENFORCED_IN_FLIGHT. Docs
   are in `26a2294` and in this file.

Decisions the integrator should know:
- Host/Origin refusals are **not** charged to the per-IP request bucket.
  They get a warning budget plus `Connection: close`. On loopback a hostile
  page and the real client share 127.0.0.1, so charging would let the page
  throttle the client.
- A WS request is bounded by the idle deadline (300 s from arrival, close
  1011). A WS write is bounded at 30 s; the close frame and socket close at
  2 s each. Shutdown waits about 4.5 s for sessions, then aborts the rest.
- `Throttled` gained a `Saturated` variant (table full of live lockouts:
  fail closed). `Backend` and `Session` each gained `max_message_bytes()`.
  `NetworkConnection::open` takes a raw upload cap. `ResolvedOptions` gained
  `remote`.
- The refusal-log test runs in its own binary (`dictate-server/tests/refusal_logs.rs`).
  A thread-scoped subscriber in the shared transport binary lost events to
  tracing's per-callsite interest cache, which other test threads populate.

Gate: `just check-cpu` (`-j 8`) at `ee86d22` passed 1146, failed 0, ignored 0. All three
clippy lines, fmt, the dictate-cli tree check and `scripts/cutover-test.sh` are
clean. No CUDA, no `just e2e`. The STT path is untouched; the pipeline change
is only to log statements.

Follow-up filed: #1522 (the local socket checks `[upload] max_bytes` against
the encoded size).
