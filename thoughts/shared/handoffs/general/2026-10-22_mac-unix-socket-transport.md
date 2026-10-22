# Mac private Unix-socket transport — #3600

Implementation source: `5371e23295c7684ab0054dc80d90872905d79b05`, based on
integration commit `02c95a8f8b6ecdabb3c3d51e0ba3dfff749a885d`.
Assigned branch: `rsi/0218a8b9-c2ba-444d-a0f1-2570953b40c3`.

The transport and live acceptance checks are complete. The required durable
CPU gate remains pending; this checkpoint does not claim a green hub gate.

## Change and decision

The hub requests `-R <mac-socket>:127.0.0.1:7313`, creating a Unix listener
in the Mac user's owned 0700 directory. It never requests a Mac TCP listener.
This removes the running-sshd/GatewayPorts exposure in #3555 and the
post-start checker in #3599. The old on-disk sshd-policy scan and lsof
post-check are gone. Preparation has a local deadline; unsafe paths and
additional configured SSH forwards are refused. Stale socket removal supports
StreamLocalBindUnlink=no. The wrapper reaps its SSH child and preserves exit
0 for stop, 1 for retry, and 78 for configuration refusal.

The Mac's `HubClient` selects Network.framework HTTP/1.1 over UDS for the
`tunnel` endpoint and URLSession for custom HTTP(S) URLs. UDS requests send
Host 127.0.0.1:7313, Content-Length and Connection: close. Response parsing
bounds headers/body and supports content-length, chunked and EOF framing;
requests have a wall-clock deadline and cancellation closes the connection.
Control bytes are rejected before request serialization. File mode,
onboarding and Test connection share this client.

Product choice: migrate only the exact former default loopback URL (also
with a trailing slash); preserve custom endpoint choices. Settings shows
“Hub (secure tunnel)”. A custom `unix:/absolute/path` supports isolated
checks without changing saved settings. A missing tunnel is an error; the
client does not fall back to TCP.

## Files

- Hub wrapper, systemd unit, Makefile/justfile installation and test recipes,
  doctor tunnel wording, API example comments.
- Mac request builder, transport, settings migration and Settings UI;
  UDS/HTTP parser tests and the shared stub server.
- Hermetic hub tests, real isolated-sshd test, installer smoke and full
  isolated Mac transport smoke.
- docs/MAC-APP.md, CLAUDE.md, mac/README.md and the S33 design amendment.
- .rsi/jobs.toml adds repeatable Mac test/verification recipes.

## Verified

- Hermetic shell checks: **35 passed, 0 failed**.
- Real isolated non-root sshd: **8 passed, 0 failed**, including GatewayPorts
  yes with UDS-only forwarding, traffic, 0700/0600 permissions, stale restart,
  actual stream-local refusal and configured-forward refusal even when the
  alias sets ClearAllForwardings=yes.
- Mac `remote.sh test`: **83 tests passed**, 16 suites. UDS transcript,
  status, 401, timeout, missing socket, cancellation, HTTP framing/limits,
  migration and custom URLSession selection all pass.
- `remote.sh dr-check`: two differing code hashes, identical certificate-based
  designated requirement. `remote.sh install`: installed and launched the
  app successfully using endpoint=tunnel. No TCC or login-keychain changes.
- `scripts/install-smoke.sh`: PASS in private HOME/DESTDIR; no real unit
  installed. Temporary-copy systemd verification, bash syntax, rustfmt and
  diff whitespace checks pass.
- Full isolated live smoke: PASS. Current rebuilt dictated used private XDG
  directories, disabled capture and loopback port 17313. A second daemon
  survived the occupied API port, reported API DOWN and transcribed locally.
  A throwaway Mac socket in a 0700 directory served authenticated status and
  transcription; missing/wrong credentials returned 401, including upload.
  Token provisioning verified 0600 and a matching value without printing it.
  No Mac TCP listener existed on the test port. The installed app's file mode
  returned ok=true over the UDS override. Killing the transient tunnel led to
  restart, stale-socket replacement and another status 200; stopping it broke
  connectivity. Cleanup removed both isolated daemons, the transient unit and
  the Mac temporary directory; hub port closure and remote-directory absence
  were independently checked.
- The successful live run used the installed CLI against explicit isolated
  sockets while the sandbox CLI cache was being rebuilt; no CLI source was
  changed by this task. Logs are local /tmp/3600-{mac-test,mac-dr,mac-install,
  shell,sshd,live}.log and are not committed.

## Hub gate — pending

Durable check-cpu attempts encountered admission congestion (#3551). One
admitted run rebuilt dictated but failed 13 CLI integration tests because a
seeded test executable embedded a missing target-pool CARGO_BIN_EXE path
(#3635). Only the sandbox's generated dictate-cli artifacts were cleaned;
the full durable gate was resubmitted. Record the successful job and complete
Rust counts here before reporting completion.

CUDA gates remain integrator work: this change does not alter STT, the
transcription path or CUDA configuration.

## Operator steps after integration

1. Install the integrated hub binaries. At a quiet moment enable the existing
   loopback [api] example, validate config and restart the live daemon. This
   session never changed or restarted it.
2. Reinstall/enable the updated tunnel unit through the existing recipe. Old
   DICTATE_MAC_PORT and sshd/lsof overrides no longer apply; default socket
   paths align automatically. A custom socket needs an existing owned parent;
   a custom hub port needs 127.0.0.1:7313 in allowed_hosts for the UDS Host.
3. Provision the real token with `just mac-token` after landing. This session
   provisioned only throwaway credentials, and left the real token untouched.
4. The Mac app has already been reinstalled with stable signing. Use its Test
   connection or the updated curl --unix-socket checks. Existing microphone
   and Accessibility onboarding requirements remain; no grants were changed.

No live dictated signals/configuration, real tunnel installation, firewall
changes, real API-token output, operator login-keychain/TCC changes or Mac
RSI daemon changes occurred. The pre-existing .rsi-build-env.json remains
untracked and untouched.

Friction: #3603, #3551, #3631, #3635
