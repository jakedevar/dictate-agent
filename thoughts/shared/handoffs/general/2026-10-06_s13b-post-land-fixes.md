# S13b post-land fixes — Issue #1478

Implementation: `9d2c4b19c7d074e53088e36c5cfb09830018ac63`, on the assigned
`rsi/f133b34c-0964-4bad-b0db-9385c2a3c888` branch, based on `ed1ef42`.
Fixture readiness follow-up: `4c01bd35b98bc9ddb25d3e2ecacd42cc8379cd09`.
No push, branch change, real-display injection, real clipboard access, or CUDA build.

The X11 selection service snapshots every available payload target as raw bytes,
including its property type and 8/16/32-bit format. Restoration changes the
payload served by the existing owner rather than reclaiming the selection.
A concurrent copy wins; empty restoration checks ownership under a server grab.
The snapshot-to-claim owner check is also atomic relative to other clients.

Consumption is explicitly a heuristic: a successful content transfer to a
requestor whose X11 resource client base matches the destination window. A
clipboard manager on another client cannot acknowledge delivery. A destination
using a separate clipboard connection may conservatively time out. After an
attempted Ctrl+V, errors and timeout never restore old clipboard contents or
retry through typing; the dictation stays served until ownership changes.
Restoration failure after a confirmed transfer never enables duplicate typing.

STRING is Latin-1 and is advertised only when representable; TEXT returns
UTF8_STRING; MIME requests retain their requested property type; TARGETS uses
ATOM/32. Legacy requests with property NONE use the target as the property.
Unknown ordinary targets, URI file lists and PNG bytes are preserved. Protocol
operations (MULTIPLE, TIMESTAMP, SAVE_TARGETS, DELETE, INSERT_SELECTION and
INSERT_PROPERTY) are not snapshotted as data. INCR and oversized properties are
refused before ownership or key injection changes; the prior clipboard remains
intact and the failure is returned with the transcript. Refused aliases in an
owner's advertised TARGETS have no available payload to preserve.

Live trusted sessions share a stop-time X11 identity between engine and pipeline.
Explicit stop captures before waking the pipeline; automatic stop fills the same
slot. Uploads do not bind or read host focus. The capture worker fails closed
within 8 ms. Injection checks the recorded window; a mismatch copies the dictation
and returns `InjectionFailed("Dictation copied: focus changed")`. XTest paste
validation and key delivery run on the same connection under a server grab.
Direct typing checks focus before backend setup and each configured chunk.
History records `output_typed=false` plus the error summary; privacy stores no row.

The ordinary desktop error notifier copies errors to the clipboard. To preserve
retained dictation or concurrent copies, injection failures use the new standalone
`Notice::InjectionFailed` notification instead. This required narrow changes in
`ports.rs` and `notify.rs` in addition to the owned engine/pipeline binding.

S25 handoff: existing Injector/TextInjector methods are intact. The new
`X11Injector::inject_bound_blocking(text, policy, Option<Option<u32>>)` and core
`TextInjector::{capture_destination, inject_bound}` seams are additive. The
pipeline dispatch arms and EDIT route were not changed. Shared merge regions are
ResolvedOptions, HostInjector/Notice, and the post-dispatch history outcome block.

| Key | Commit | Regression |
| --- | --- | --- |
| PASTE_CLIPBOARD_MANAGER_FALSE_CONSUME | 9d2c4b19c7d074e53088e36c5cfb09830018ac63 | clipboard_manager_cannot_acknowledge_destination_consumption |
| CLIPBOARD_RESTORE_OVERWRITES_CONCURRENT_COPY | 9d2c4b19c7d074e53088e36c5cfb09830018ac63 | concurrent_copy_wins_over_restore_and_snapshot_claim (including SelectionClear) |
| PASTE_TIMEOUT_THEN_LATE_PASTE_OF_OLD_CLIPBOARD | 9d2c4b19c7d074e53088e36c5cfb09830018ac63 | timed_out_paste_keeps_dictation_for_late_request; xterm SIGSTOP for 2300 ms, with extra-character detection |
| PASTE_TARGET_COVERAGE_GAPS | 9d2c4b19c7d074e53088e36c5cfb09830018ac63 | target_types_legacy_property_and_raw_snapshot_are_preserved; latin1_string_is_not_advertised_for_unrepresentable_unicode |
| STOP_DESTINATION_NOT_BOUND_TO_INJECTION | 9d2c4b19c7d074e53088e36c5cfb09830018ac63 | changed_focus_sends_no_paste_keys; xterm focus-paste/focus-type; explicit_stop_binds_delivery_and_records_clipboard_only_outcome (including privacy) |
| TEST_GAPS_X11_SMOKE | 9d2c4b19c7d074e53088e36c5cfb09830018ac63; 4c01bd35b98bc9ddb25d3e2ecacd42cc8379cd09 | seven private-Xvfb raw tests; eleven xterm cases; isolated subprocess DISPLAY with WAYLAND_DISPLAY/XAUTHORITY removed |
| CLIPBOARD_SNAPSHOT_DROPS_UNSUPPORTED_TARGETS | 9d2c4b19c7d074e53088e36c5cfb09830018ac63 | raw 16-bit target plus URI list round trip; PNG byte comparison; incr_and_oversized_payloads_fail_before_claiming_or_sending_keys |

Validation (CARGO_BUILD_JOBS=8; one active build; load below 40):

- `just check-cpu`: **912 passed / 0 failed / 0 ignored**, all three clippy
  variants, fmt and CLI dependency check passed. Log `/tmp/s13b-final-check-cpu.log`.
- `cargo test -j 8 -p dictate-inject --features x11-tests --all-targets`:
  **13 passed / 0 failed** (12 unit tests and one smoke test). The subprocess's
  second smoke result is the same test and is not counted twice.
  Log `/tmp/s13b-final-x11-tests.log`. Seven tests supplement the default gate,
  for **919 distinct tests passed / 0 failed** across the two configurations.
- `cargo clippy -j 8 -p dictate-inject --features x11-tests --all-targets -- -D warnings`:
  passed; log `/tmp/s13b-x11-clippy.log`.
- Smoke covers empty/text/PNG/HTML, 14,999 characters, 150 ms delay, direct typing,
  ignored paste timeout, a 2300 ms late paste, and both focus-change policies.
  Raw tests use their own Xvfb connections, without changing process environment.

During implementation, the smoke caught arboard's advertised-but-refused UTF-8
aliases and a fixture race reading the output file before printf finished.
Snapshotting now handles refused aliases, and read-back waits for the complete
payload. A post-commit run also caught xclip's launcher exiting before its PNG
selection owner was ready; the fixture now waits for an image within a fixed
two-second deadline. All affected tests and both clippy configurations were
rerun successfully after that fix. No assertion was weakened. Two obsolete tests expecting paste
errors to enable typing were replaced by the private-Xvfb safety regressions.

Friction: #1482
