# Issue #1508: repeated upload stall

Code commit: `86a77b7c246847348484cafb0620734bac63601b`.
Base: `6db721f05b186bae5b1fbd66ec44a3191132b6da`.

## Root cause

`connection_loop` selects between `read_line_bounded` and engine events.
The latter runs even when a connection is not subscribed. The original
reader kept its partial frame in a local vector. If a broadcast arrived
after the reader consumed a fragment of the next large upload, `select!`
dropped the read future and its bytes. The next iteration parsed only the
remaining suffix, emitted a malformed-request event without a request ID,
and left the client waiting for its response until the 10-second timeout.
Completion/idle events from the previous upload make immediate repeats
particularly vulnerable.

The engine queues session completion before delivering the upload outcome,
ignores stale completions by session ID, and refuses only a live slot. The
hang-up watcher preserves read-ahead in `ConnReader`. `CancelToken` registers
its notification before checking cancellation. Those paths require no change.

## Change and regression coverage

The frame buffer now belongs to the connection and survives event delivery.
Resumed reads subtract the already-consumed bytes from the message budget.
Complete frames clear the buffer; EOF still delivers a final partial frame.

The CPU harness uses mock VAD/STT with 20/130 ms delays. One test mirrors
the real e2e loop: three upload sizes, warm-up plus eight repeats each, on
one unsubscribed connection. A second forces an event between large request
fragments, six times on the same connection. Finishing the large prefix write
requires the server to consume an incomplete frame; receiving the marker
proves the event arm ran before the suffix was sent.

Three unit tests deterministically poll and drop an incomplete read, checking
frame preservation and the next line, whole-frame size limits over multiple
interruptions, and EOF after interruption. Existing pipeline/hang-up tests
continue to cover cancellation and pipelined request ordering.

## Verification

- Original production reader with the new CPU regression: 0 passed / 1 failed,
  `harness/mod.rs:59` timeout waiting for responses, 10.22 seconds.
  Log: `/tmp/issue-1508-repro.log`.
- Fixed focused upload suite: 26 passed / 0 failed.
  Log: `/tmp/issue-1508-upload.log`.
- Fixed server unit tests: 20 passed / 0 failed.
  Log: `/tmp/issue-1508-framing.log`.
- `CARGO_BUILD_JOBS=8 just check-cpu`: passed, 1,041 top-level Rust tests and
  23 cutover tests, zero failures; all three Clippy lines, fmt and CLI tree
  check passed. The Rust log also contains two successful private-Xvfb child
  summaries, excluded from the top-level count.
  Log: `/tmp/issue-1508-check-cpu.log`.
- `CARGO_BUILD_JOBS=8 just e2e`, looped ten times: 10 consecutive green full
  suites, 70 passed / 0 failed. Default `E2E_RUNS=8`: 27 headline uploads per
  suite, 270 in total. Every short/medium/long fixture summary reported WER
  0.000; no stalls. Per-suite runtime after the cold release/CUDA build was
  9.02–12.02 seconds. Logs: `/tmp/issue-1508-e2e-{1..10}.log`.
  The runner checked `uptime` before every run and required one-minute load
  at most 40. Build jobs were capped at eight.

Acceptance gate total: 1,134 passed / 0 failed (1,041 Rust + 23 cutover +
70 real-hardware tests). The focused suites above are additional executions,
not included again in that gate total.

Only `crates/dictated/src/server.rs` and `crates/dictated/tests/upload.rs`
changed in the code commit. No live daemon or real display was used, and
nothing was pushed.

Friction: none
