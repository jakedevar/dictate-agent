---
date: 2026-10-23
issue: 3961
plan: 3943
session: d3d462c4-f62e-434c-9fd2-b0cbe9f85848
author_model: Codex gpt-6.1-sol high
base_commit: c50f0e2752e3281038f5031407c900bc2cc0a391
---

# DictateHub local-first endpoint selection

Auto tries the Mac dictated socket before the hub tunnel. Mac only, Hub
only and Custom URL select exactly one endpoint. A legacy saved tunnel or
loopback default migrates to Auto; an explicitly selected Hub only persists.
The local token is read directly on every request and never copied, written,
chmodded or logged. A working Mac needs no hub token.

The UDS transport marks a request started before handing bytes to
Network.framework. Missing/refused sockets and pre-send timeouts are typed
safe-to-fallback failures. A local HTTP 503 is treated as an explicit
not-ready rejection. A 401, unknown error, invalid response, failed send or
post-send timeout cannot trigger another upload. This is deliberately
conservative when Network.framework cannot prove whether bytes arrived.
An absent socket and absent token may fall back; an existing socket with
no local token fails locally. Permission and other filesystem errors are
not treated as proof of an absent socket.

Settings and the status item tooltip name the last dictation endpoint.
Launch logs record local/hub/hub-fallback with no dictation text. Connection
tests probe both endpoints independently, with at most one local request,
and do not change the last dictation endpoint. Local 401s say "local token
rejected". Onboarding recognizes the local token.

The second-client wrapper sends a WAV with curl over the local UDS and puts
the bearer header only on stdin via `-K -`. It refuses config metacharacters,
propagates curl errors, and never retries. Its fixed Host header matches the
Swift transport and default S33 allow-list. Using curl's default Host for
`http://localhost` initially returned 421 from the real daemon; the corrected
single read-only status probe returned 200. Unit tests continue to use stubs.

Product choice: Auto allows the hub to receive audio when the local daemon
is unavailable; Mac only provides an explicit local-only choice. A 503 is
the server's rejection signal, whereas ambiguous post-send failures are
shown to the user without retry. Custom URLs continue using the hub token.

# Verification

Exact command, run on the Mac through ssh mac-mini:

```sh
DICTATE_MAC_BUILD_ID=d3d462c4-f62e-434c-9fd2-b0cbe9f85848 mac/scripts/remote.sh test
```

Exit 0. Final log: `/tmp/3961-mac-test-acceptance.log` on the hub.

- Swift Testing: 132 tests in 26 suites passed, 0 failed.
- Existing install guard: 59 passed, 0 failed (temporary destinations only).
- Second-client shell checks: 12 passed, 0 failed.
- Total reported checks: 203 passed, 0 failed.
- Actual UDS stubs verify local-up/hub-down and the reverse, distinct tokens,
  401/503 decisions, post-send timeout without a second upload, stale refused
  socket, token rotation, and the shell wrapper's body/auth/Host framing.
- Decision tests cover pre-send timeout, malformed response and other unsafe
  failures. Probe tests assert exactly one request to each endpoint.
- App executable and tests compile as part of swift test. Shell syntax and
  git diff whitespace checks passed. No Rust changes or Rust gate run.

The initial durable mac-test job failed on a test helper access modifier,
fixed before the passing runs. It also dropped RSI_SESSION_ID and synced to
manual.noindex; #3966 in the RSI harness project records the isolation defect.
All acceptance runs explicitly set the session build ID and used its own
`.noindex` directory.

# PM install notes

The full final tip SHA is published on #3961. Integrate both commits above
base c50f0e2752e3281038f5031407c900bc2cc0a391 from the shared object store
(the initial checkpoint and the follow-up), rather than cherry-picking only
the final follow-up. Nothing was pushed, following the explicit slice
brief and worker-contract push exception (#3542).

After integration, the PM can build/install from the integration checkout:

```sh
DICTATE_MAC_BUILD_ID=pm-3961 mac/scripts/remote.sh install
```

The PM should confirm Auto in Settings, Test connection's separate Mac and
hub results, then a hotkey dictation with `Using: Mac (local)` and a local
endpoint launch.log entry. Hub fallback was exercised against stubs; the
hub dictated.service remains deliberately stopped. Do not restart it just
to install this client. Local dictated provisioning/LaunchAgent belongs to
#3945; this slice does not change it. No app install, /Applications bundle,
TCC, LaunchAgent or hub service was touched by this worker. Interactive
hotkey/paste acceptance remains the PM's post-install check.

Only Swift and shell under mac/, the requested docs/MAC-APP.md, and this
required handoff artifact changed. There were no edits to the parallel Rust
dictated slice. No product decision requires an operator reply.

RSI's ancestry hook refused amending the unpublished checkpoint; the final
changes are a follow-up commit. #3967 in the RSI harness project requests
that this restriction be documented upfront.

Friction: #3542, #3966, #3967 (last two in RSI harness)
