# Hub GPU coordination (protocol v1)

The hub's `dictated.service` runs `dictate-gpu-gate acquire` before loading its
model. This gives PlayboyAI's execution gate time to suspend GPU work. Neither
helper links audio, Whisper, CUDA or an async runtime. This applies to the Linux
hub only; the Mac-local daemon is outside this protocol.

`dictate-gpu-status.socket` listens at
`$XDG_RUNTIME_DIR/dictate-agent/gpu-status.sock` (0600, parent directory 0700).
Systemd accepts each connection and runs `dictate-gpu-status` with the accepted
socket as stdin/stdout. The client receives exactly one JSON line immediately;
it may send `{"op":"status"}` but need not send anything. The responder never
starts dictated, and its units have no dependency that starts dictated.

```json
{"v":1,"state":"off","since":"2026-10-24T00:00:00Z","observed_at":"2026-10-24T00:00:01Z","sources":{"dictated_service":"inactive","legacy_dictate_agent":"absent"},"gpu":{"cuda":false,"model_loaded":false}}
```

The observer reads systemd's ActiveState and transition timestamps, checks the
legacy PID file and `/proc/*/comm`, and, only for active dictated, handshakes and
requests `get_status` on its control socket. Each systemd/control probe has a
750 ms deadline. Replies are capped at 64 KiB and control socket I/O uses an
absolute deadline, including connect and handshake. A missing/unresponsive
control socket means active-but-not-ready (`starting`). A failed systemd probe
is reported as source `failed` and logged to stderr. No source probe changes
service state, configuration or models.

State is `on` if the legacy process is running, or dictated is active and its
model is loaded; `starting` if activating or active without a loaded model;
otherwise `off`. CPU-ready dictated is also `on`, with `gpu.cuda=false`.
GPU flags reflect dictated's `get_status` only; the legacy binary cannot report
backend/readiness, so legacy-only `on` retains false flags. Consumers must use
`state` for exclusion rather than interpreting false flags as permission.
Because dictated also claims `dictate.pid`, the PID must identify a live
`dictate-agent`, not merely a live process. The comm scan also finds a legacy
process without its PID file. `since` uses systemd's active-entry or state-change
time; it is a service transition estimate, not a model-ready timestamp. For a
legacy observation or unavailable timestamp it equals `observed_at`.

The start interlock connects to `$XDG_RUNTIME_DIR/playboy/control.sock` and sends:

```json
{"op":"gpu_gate_acquire","v":1,"holder":"dictation","intent_id":"<fresh v4 UUID>","deadline_ms":90000}
```

It waits for one JSON line containing boolean `granted`. A grant permits the
start after the reply. ENOENT/ECONNREFUSED proceeds immediately; silence proceeds
at the deadline with `gate_timeout`; malformed, oversized, closed, refused or
other error replies proceed with a warning. Every normal invocation exits zero
and emits one journal line. `TimeoutStartSec=180` leaves room for the 90-second
gate plus model startup. This is deliberately fail-open: dictation remains
available if PlayboyAI is unavailable. The handshake cannot guarantee mutual
exclusion after a fail-open outcome; PlayboyAI must also consume the status.

Overrides (environment takes priority over config):

| Setting | Default | Environment |
| --- | --- | --- |
| socket | `$XDG_RUNTIME_DIR/playboy/control.sock` | `DICTATE_GPU_GATE_SOCKET` |
| op | `gpu_gate_acquire` | `DICTATE_GPU_GATE_OP` |
| deadline_ms | `90000` | `DICTATE_GPU_GATE_DEADLINE_MS` |

`DICTATE_GPU_GATE_CONFIG` optionally names a dedicated TOML file with `socket`,
`op`, `deadline_ms` keys. It does not read or change dictation's shared config.
Deadlines must be 1–90000 ms; shorter values support isolated tests. Bad config
logs `gate_config_error` and permits startup. The status control-socket override
is the existing `DICTATE_SOCKET`. The default status listener path is fixed by
its socket unit; an operator may override `ListenStream` with a drop-in.
Read-only fixture overrides `DICTATE_GPU_STATUS_LEGACY_PID` and
`DICTATE_GPU_STATUS_PROC_ROOT` isolate PID and proc inputs in tests; the
systemctl executable is resolved through PATH.

`scripts/run.sh` refuses the retained legacy binary unless
`DICTATE_ALLOW_LEGACY=1`. When allowed and dictated is not enabled, it runs the
gate before launching the legacy binary. If the helper is absent, the legacy
launcher refuses rather than silently bypassing it. The old installed binary
is retained. The Mac DictateHub fallback cannot start hub dictation: when the
hub daemon is stopped, its tunnel connection is refused; it does not invoke a
remote service start and needs no separate interlock.

## PM installation (worker must not run these live)

Build the helpers with `cargo build --release -p dictate-gpu-gate`. Install both
`dictate-gpu-gate` and `dictate-gpu-status` from the selected cargo target's
`release/` into `~/.local/bin/` (0755). No CUDA rebuild is needed for these
helpers. Install the updated `systemd/dictated.service`,
`systemd/dictate-gpu-status.socket` and `systemd/dictate-gpu-status@.service` into
`~/.config/systemd/user/` (0644). Keep any intentional local dictated overrides.
`make install-files install-unit` also installs the helpers and units, supports
DESTDIR staging and alternate prefixes, and does not reload or start services.
The release tarball carries the same files.

Then run **only**:

```sh
systemctl --user daemon-reload
systemctl --user enable --now dictate-gpu-status.socket
```

Leave `dictated.service` stopped. Do not enable/start/restart it for this
installation. Querying the status socket should report `off` and must leave it
stopped. The updated unit gates the next separately authorized start, boot or
restart. If protocol changes arrive, update the constants/overrides before
that next start. All worker tests use stub UDS peers and staged units; none
contact PlayboyAI or operate live hub services.
