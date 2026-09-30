# S13b — injection fidelity, stop-time destination, STT backend truth

Implemented on the assigned RSI branch from integration tip
`477e17129adf78a0c525b6373b852de36c59e02e`. Issues 1067, 1065 and 1068
are resolved here. No changes to the live daemon, microphone, user configuration
or user databases; all committed fixtures are synthetic.

## Clipboard delivery (1067)

`X11Injector` saves image pixels through arboard first, otherwise text and
HTML (including the plain-text alternative), otherwise emptiness. Missing
content is an ordinary snapshot; transport/conversion errors remain visible
and prevent unsafe replacement. PNG dimensions and RGBA pixels round-trip
exactly; PNG encoding bytes need not remain identical. Text/HTML retain their
contents, including Unicode and newlines.

A temporary x11rb selection owner serves the dictation. The application asks
for UTF-8 data, which is written into the requestor's own X11 property before
SelectionNotify. Only then is the old clipboard restored. This replaces the
50 ms guess: restored selection contents cannot change the transferred
property, even if the application reads it later. The injector retains the
arboard clipboard owner for its lifetime, so restored content survives without
a desktop clipboard manager. Transactions are serialized across injector clones.

Unsupported/ignored Ctrl+V times out after two seconds and uses existing
Unicode chunked typing; an oversized single-property payload also uses typing,
without truncation. No typing retry follows a successful transfer whose
clipboard restoration fails; that case warns and reports successful delivery.
Cancellation stays at the existing injection commit point. No new backends.

**Consequential format decision:** arboard can restore one image, text, or
HTML with optional text. Arbitrary X11 targets (file lists, opaque application
formats, original target aliases, mixed image/text bundles) are not faithfully
round-tripped. Image pixels take priority over textual labels. An opaque-only
selection restores as empty. The first paste in each process issues an explicit
WARN documenting this limitation; the handoff does not claim universal X11
clipboard preservation. HTML/text and PNG support cover the required cases
without adding an unbounded generic selection/INCR capture implementation.

Private Xvfb + xterm + xclip smoke (never :0), one test / eight cases:

| Prior content / condition | Delivery | Paste + readback + restore |
| --- | --- | ---: |
| Empty | Paste, restored empty | 13.97 ms |
| Unicode text with newline | Paste, restored exactly | 12.77 ms |
| `xclip -selection clipboard -t image/png` | Paste, dimensions/pixels identical | 14.74 ms |
| HTML with text alternative | Paste, both restored exactly | 12.68 ms |
| 1,500 words / 14,999 characters | Paste, complete readback, clipboard restored | 12.65 ms |
| Consumer suspended for 150 ms | Paste, complete readback, clipboard restored | 162.44 ms |
| Explicit typing policy | Keystroke, clipboard unchanged | 11.28 ms |
| Ctrl+V explicitly ignored | Keystroke fallback, clipboard restored | 2,013.29 ms |

The suspended-consumer case proves restoration follows the selection request,
not an arbitrary sleep. Unit tests pin successful-delivery/failed-restoration
handling and failed-paste fallback with preserved error details.

## Destination policy (1065)

**Manager's product decision:** the app at recording stop expresses intent.
A user may start talking, click into a destination, then end the dictation.
Injection continues to go to whatever window is focused at delivery; it never
re-focuses or refuses a window.

Start capture remains before audio.start. The engine captures the bounded
context synchronously at explicit Stop (including toggle and hold release),
and stores it on the shared session handle before notifying the pipeline.
This matters if media pause delays the session task. VAD auto-stop captures
immediately after its trailing-silence decision. When the app ID differs,
the pipeline replaces context/profile once and publishes ContextResolved
again before Transcribing. An unchanged app retains its original profile,
including title-dependent decisions. History and dictionary scope use the
stop-time app. S20/S21 can consume the updated existing `opts.profile` and
`opts.context`; their formatter region/signatures were not changed here.

Tests cover toggle, PushToTalk release via Stop, OneShot and WakeWord auto-stop,
same-app title changes, the delayed-media race, private history, named apps,
remote event filtering, and uploads that panic on any host focus read.
S23/protocol comments promising start-only immutability were updated; no wire
shape changed, and protocol golden tests pass.

## Actual STT backend (1068)

The process-lifetime whisper-rs logging callback collects initialization
messages in thread-local, per-load evidence. Complete lines are parsed even
when callbacks split messages. The pinned whisper.cpp implementation announces
`whisper_backend_init_gpu: using CUDA0 backend` before initializing that device;
a following initialization failure or `no GPU found` resolves to CPU. The
loaded context and state must succeed before this evidence is published.
Configuration, device enumeration and CUDA host buffers are not evidence of
GPU selection. Missing/unrecognized evidence stays unknown; it never echoes
the request. A backend mismatch issues WARN and is exposed by doctor.

The callback preserves warnings/errors in tracing and sends informational
whisper/ggml chatter to debug. It catches unwinding across the FFI boundary.
Concurrent providers cannot mix their load evidence. The logging callback is
process-global; future integrations that replace it must retain observation.
Five unit tests need no model or GPU. Real hardware tests verify CPU selection,
CUDA selection, and a fresh child process with CUDA_VISIBLE_DEVICES=-1 that
requests CUDA: observed backend cpu, WARN captured, doctor reports
`configured for cuda but running on cpu`. This last test fails with the old
configuration-echo behavior.

## Verification observed to finish

All commands used `/opt/cuda/bin` on PATH. Logs are local under `/tmp/S13b-*`.

- Baseline `cargo test --workspace --all-targets`: **616 passed / 0 failed**.
- Final workspace all-targets: **627 passed / 0 failed** (11 added tests).
- Private Xvfb smoke with nocapture: **1 passed / 0 failed**, eight cases above.
- `just e2e`: **7 passed / 0 failed**, including the new CPU and forced-fallback tests.
- Workspace all-targets clippy with `-D warnings`: passed.
- `cargo clippy -p dictated --features e2e-real --all-targets -- -D warnings`: passed.
- `cargo clippy -p dictate-context --features x11-tests --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- CLI normal dependency tree search for `whisper|dictate-fmt`: empty.
- `git diff --check`: passed.

Final `just e2e` on the RTX 5080 (eight measured runs per fixture):

| Synthetic fixture | Audio | STT p50 / p95 | Total p50 / p95 | WER |
| --- | ---: | ---: | ---: | ---: |
| Short | 3.3 s | 105.1 / 106.7 ms | 111.1 / 112.7 ms | 0.000 |
| Medium | 9.3 s | 135.8 / 138.6 ms | 151.8 / 154.4 ms | 0.000 |
| Long | 33.3 s | 333.5 / 339.6 ms | 390.3 / 396.5 ms | 0.000 |

Observed backend cuda, cold ready 190 ms, benchmark process GPU memory 2,356 MiB;
doctor separately confirmed 2,200 MiB and pinned model SHA-256. Cancellation
left the next upload usable (369 ms, WER 0.000). Doctor's existing local-model
warning remains (qwen3:14b missing); headless injection warning is expected in
this isolated mock-injector E2E. These are not STT test failures. The fixtures
validate plumbing/basic health, not real-voice accuracy.

## Integration notes

Commits are logical checkpoints on the assigned branch; nothing pushed.
`pipeline.rs` has only options/start-stop handling plus a dictionary-scope
comment change (25-line diff); the formatting region was neither moved nor
reformatted. `dictate-stt/src/transcribe.rs` changes only backend load/status,
leaving S20's correction-removal region intact.

Other shared files touched: engine/session stop bookkeeping, Cargo.lock (one
x11rb dependency), protocol/context documentation, doctor comments, and
additive `dictated/tests/e2e_real.rs` hardware tests. No Formatter port changes,
new protocol fields, or production config edits. No follow-up Issues filed;
no additional product decision needed. Independent review/integration remain
with the appointed manager.
