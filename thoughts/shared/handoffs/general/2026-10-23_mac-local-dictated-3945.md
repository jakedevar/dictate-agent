# #3945: Mac-local dictated, Metal, private S33 HTTP socket, LaunchAgent

Author: Codex, gpt-6.1-sol, high (RSI session bcbbc7c6-1c22-4cc2-917f-58af730e6bc9).
Base: master 5b36baf. Runtime/test source: e63bf50df347fd2805ab1b48d6e508889b39c434.
This final artifact commit adds documentation only. Assigned branch remains
rsi/bcbbc7c6-1c22-4cc2-917f-58af730e6bc9. No push, following the explicit
slice brief and worker-contract.md section 1.2 (shared object store).

Implemented and installed on mac-mini. The entire workspace builds in release
with dictated/metal; the installed service serves a synthetic WAV through S33
on the private HTTP Unix socket, with actual model status backend=metal.
The hub dictated.service and tunnel were never touched. DictateHub.app,
settings and TCC were untouched. No Python was written or run.

## Engineering decisions and assumptions

- Reuse the existing whisper-rs provider and deterministic text chain. The
  feature chain is dictated/metal -> dictate-core/metal -> dictate-stt/metal
  -> whisper-rs/metal. GPU use is enabled for either CUDA or Metal requests;
  backend status remains observation of whisper.cpp, not the requested flag.
- Keep the model configurable. Initial Mac config uses large-v3-turbo,
  device=metal, and the pinned catalog model under Application Support.
  #3944 selects any later model change; this slice makes no benchmark or
  hub-parity claim from a two-request smoke check.
- UDS mode is exclusive. api.socket replaces TCP even with the historical
  default bind inherited; bind="" is also allowed. A different explicit TCP
  bind, TLS keys or LAN flags alongside socket are refused. There is no
  fallback TCP listener on failure. The Mac production runner also refuses
  an enabled API without api.socket; leaf transport tests can still test TCP.
- Serve the same axum router and NetworkBackend. Unix HTTP never becomes the
  privileged NDJSON control grant. Bearer auth, Host 127.0.0.1:7313 (or its
  existing localhost form), Origin checks, rate/body/session limits and the
  transcription-only ceiling remain. UDS peers share one throttle bucket.
- Socket custody walks every parent with openat(O_DIRECTORY|O_NOFOLLOW),
  creates missing directories 0700, checks ownership and ancestor write
  permissions, and fchmods the final owned directory. Root-owned sticky
  ancestors support /tmp; symlink ancestors are refused (Darwin test fixtures
  canonicalize /tmp before choosing a socket path). Hold a private .api.lock
  flock for the listener lifetime. Refuse files, symlinks, non-owned sockets,
  live sockets or uncertain liveness; only an owned connection-refused socket
  may be reclaimed. Socket mode is 0600 inside the already-private directory;
  the process umask is never changed. Drop unlinks only the inode we bound.
  Same-UID compromise remains outside S33's threat model, as in its design.
- Mac production forces microphone capture, injection, notifications and
  evdev off. X11/evdev implementations and dependencies are Linux-gated;
  other hosts get unavailable injection/context and no-op media/notification
  adapters. Legacy signal PID claiming is Linux-only. The private NDJSON
  control socket still exists separately from S33; HTTP has no local grant.
- Runtime/config/token/history and models live under
  ~/Library/Application Support/dictated; run/api.sock is the HTTP endpoint.
  The model-management CLI uses the same Mac model directory. Token issuance
  and rotation work without restarting and print a Unix endpoint on stderr.
- Use a real ~/Library/LaunchAgents/dev.jakedevar.dictated.plist, KeepAlive,
  RunAtLoad and logs under ~/Library/Logs/dictated. Fresh install bootstraps
  gui/$UID; reinstall preserves config and restarts the registered job with
  kickstart -k. Job properties are fixed by this script; binary/config contents
  can change. Uninstall bootouts and removes only the plist, retaining state.
- Build source is isolated under ~/dictate-mac-build/<ID>.noindex/rust.
  ID precedence: DICTATE_MAC_BUILD_ID, RSI_SESSION_ID, assigned rsi/ branch
  suffix, then manual for a normal non-RSI checkout. Nice 10, at most 6 jobs.
  The initial task-created manual Rust directory was moved to this session's
  UUID directory; no other worker's directory was altered (RSI #3949).
- Keep Linux-specific backlog regression coverage on Linux; Darwin gets a
  bounded stale-socket diagnostic fixture and libproc process names. The
  pipelined-hangup fixture explicitly requests its send buffer capacity so
  the original read-ahead-bound and no-injection assertions work on Darwin.
  Concurrent config fixtures use a PID plus atomic sequence (#3960 audit).

## Final verification

All requested test jobs omitted timeout_minutes in their submitted params.

| Check | Result | RSI job |
|---|---|---|
| Linux just check-cpu | 1168 Rust passed / 0 failed; all clippy variants, fmt, CLI dependency check passed | 2c7f421f-f668-4688-8a52-87e6d95e1135 |
| Linux just check-cuda | Both CUDA clippy variants passed | d0531c08-2fbd-4359-b317-d49ec4c8b00c |
| Mac cargo test -p dictate-server -p dictated | 330 Rust passed / 0 failed; no warnings | 349910fe-268b-4b54-acc9-68c5bcb2facd |
| LaunchAgent shell tests (stub HOME/launchctl) | 1 passed / 0 failed on Linux and Mac; includes repeated install/config preservation/no bootout | same Mac job; Linux foreground |
| Mac release --workspace --features dictated/metal and installation | passed; pinned model verified, config check clean | 9840a776-9bf2-41e3-b170-d677362a4b80 |
| Real second install of already registered job | passed over ssh at 23:23Z; launchctl state running | foreground, built binaries, existing per-session script |
| Installed second-client curl UDS smoke after second install | passed | aef75b23-73b4-42d4-aafe-8c04b1ae7bc3 |

The UDS tests cover directory/socket modes, stale/live socket handling,
symlink and unsafe-ancestor refusal, non-socket preservation, exclusive mode,
identity-safe shutdown, bearer/Origin enforcement, token rotation, real WAV
transcription, no TCP address, no injection, and forbidden history/config/
context/dictionary/diagnostic commands over a UDS WebSocket.

Final live result (synthetic crates/dictated/tests/fixtures/e2e/short.wav,
3.3043125 seconds; both POSTs HTTP 200):

```json
{"type":"transcript","text":"Please schedule the review meeting for Thursday afternoon.","route":"type","model":"large-v3-turbo"}
```

First observed request 1.674559 s; immediately repeated request 1.516238 s.
The script calls the first field cold_latency; it is the first request of this
smoke run, not a controlled cold-model benchmark. Warm server STT 1485.379125 ms,
rules 0.020917 ms, total 1514.874917 ms. LLM disabled; injection skipped as
not_permitted. Status: loaded=true, backend=metal, routes=[type], private
capabilities all false. No unauthenticated/host access was exposed:

- Missing token: 401; hostile Origin: 403; hostile Host: 421.
- Rotate via installed dictated --rotate-api-token >/dev/null: old token 401,
  new token status 200 without a restart. Credentials only passed via stdin.
- launchctl reported the real LaunchAgent; final observed PID 70924.
- lsof on that PID found zero listening TCP sockets.
- run directory 0700; run/api.sock and api-token 0600.

## Exact PM install and use steps

From the repository with this commit available and ssh mac-mini configured:

```bash
export DICTATE_MAC_BUILD_ID=pm-3945
just mac-dictated-setup
just mac-dictated-build
just mac-dictated-test
just mac-dictated-install
just mac-dictated-smoke
```

Setup installs user-local rustup if needed and Homebrew cmake without sudo.
Build/install compile the entire workspace in release with Metal. Install
pulls large-v3-turbo using the existing immutable catalog revision and SHA-256
1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69.
It stages the binary atomically to Application Support/dictated/bin/dictated,
creates config.toml only if missing, creates/checks the 0600 token with stdout
discarded, and bootstraps/kickstarts the real LaunchAgent. Smoke rotates this
local token, so a future client should read the current token after smoke.

On the Mac, from ~/dictate-mac-build/pm-3945.noindex/rust:

```bash
mac/scripts/dictated-launchagent.sh status
mac/scripts/dictated-launchagent.sh restart
# Optional removal: keeps binary, config, models and state.
mac/scripts/dictated-launchagent.sh uninstall
```

Manual second client on the Mac (a synthetic WAV path):

```bash
data="$HOME/Library/Application Support/dictated"
printf 'header = "Authorization: Bearer %s"\n' "$(cat "$data/api-token")" |
  curl --fail-with-body --silent --show-error --unix-socket "$data/run/api.sock" \
    -K - -H 'Content-Type: audio/wav' --data-binary @synthetic.wav \
    -w '\nlatency=%{time_total}s\n' http://127.0.0.1:7313/v1/transcribe
```

The service is currently left running on the Mac as requested. Its config
selects large-v3-turbo/Metal with deterministic formatting and the LLM off.
Actual paths: ~/Library/Application Support/dictated/{config.toml,api-token,
models/,history.db,bin/dictated,run/api.sock}; logs ~/Library/Logs/dictated/.
There is no change to the app endpoint settings. Product choice: keep the
single local engine and conservative deterministic-only initial config;
benchmark/model selection belongs to #3944, local-first hotkey/app and hub
fallback to plan #3943's following slice. Neither is claimed verified here.

Pre-land security review remains the manager's required next action. The
review focus is crates/dictate-server/src/{unix,serve,config}.rs and the UDS
NetworkBackend integration test; no schema migration was introduced.

Friction: #3949 (RSI project), #3960 (dictate-agent project)
