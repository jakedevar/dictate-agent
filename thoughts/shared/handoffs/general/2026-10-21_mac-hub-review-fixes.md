# Handoff: Mac app hub side, fixes for review #3553 (#3555, #3556, #3557)

Issue #3528. This follows `2026-10-22_mac-app-hub-side.md` (commit 9bf1b02),
which pre-land security review #3553 returned as changes-requested.

## Commits (on top of 9bf1b02)
- 561e204 fix(mac): the tunnel enforces loopback-only on the Mac, fail-closed (#3555), plus the docs fix (#3556) and the tests
- 36a9dc5 docs(thoughts): S33 design amendment (the loopback guard)
- 57c69ca fix(mac): token provisioning never follows a pre-planted symlink (#3557)
- this handoff

## #3555 (HIGH): loopback-only is now enforced, not assumed
`systemd/dictate-mac-tunnel.service` now starts `%h/.local/bin/dictate-mac-tunnel`
(source `scripts/dictate-mac-tunnel`; `make install-mac-tunnel` installs it to
BINDIR and rewrites ExecStart). On every start the wrapper does three things.

1. **Preflight**, over a forward-less ssh session (`ClearAllForwardings=yes`,
   BatchMode, StrictHostKeyChecking=yes, ControlPath=none):
   - `sshd -G -f <config>` gives the effective global value. It works without
     root on macOS; verified on mac-mini, which reports `gatewayports no`.
   - It also reads the text of sshd_config and every Include, recursively.
     It resolves relative paths against the config's directory, follows
     macOS's `sshd_config.d/*` → `crypto.conf` chain, de-duplicates, and
     stops at 8 rounds.
   - The text scan is needed because `sshd -G` without `-C` ignores Match
     blocks, and `-C` needs `-T`, which needs root (host keys).
   - It refuses when any GatewayPorts value anywhere is not exactly `no`:
     case-insensitive, `=` separator, quotes, inline comments, two values,
     an empty value, inside Match. It also refuses on an unreadable file, an
     Include it cannot follow (`~`, a quote), an unbalanced quote, a failing
     or missing sshd -G, a truncated reply or an unexpected line.
   - Decision: `clientspecified` is refused too, even though it would honour
     our explicit 127.0.0.1. The brief said "not `no`", and being strict
     costs nothing on macOS.
2. **Tunnel:** `ssh -N -T -R 127.0.0.1:MAC:127.0.0.1:HUB` with
   ExitOnForwardFailure, ServerAlive 15x3 and ForwardAgent/X11 no.
3. **Post-check:** one ssh session runs `lsof -nP -iTCP:P -sTCP:LISTEN -Fn` on
   the Mac, polling every 0.25 s for up to 20 s. Every listener must be
   `127.0.0.1:P` or `[::1]:P`; otherwise it kills the tunnel. Missing lsof is
   a refusal; no listener is a retry.

**Exit codes:** 78 means refused, and `RestartPreventExitStatus=78` keeps the
unit failed instead of retrying every 10 s. Network trouble exits 1, which
Restart= retries as before.

**`dictate-mac-tunnel --check`** runs only the preflight.

**Doctor:** `mac_tunnel` now reads `ExecMainStatus`. A failed unit with
status 78 is reported as "refused to start", with the fix: `--check`, then
restart.

**Stated limit** (in docs/MAC-APP.md): the preflight reads the config sshd
*would* use. If sshd runs some other config, lsof is the backstop, and the
port can be exposed for about a second before it fires.

**Rejected alternative:** a Unix-socket `-R` with a Mac-side TCP relay would
be immune to GatewayPorts, but it needs socat or python on the Mac and
changes the Mac app's contract.

## #3556: no token on any argv
- docs/MAC-APP.md's check command is now
  `printf "header = \"Authorization: Bearer %s\"\n" "$(cat …/api-token)" | curl -s -K - …`.
  `printf` is a shell builtin and `cat` sees only the path.
- Verified on mac-mini with a dummy value: macOS curl sent the header (an
  `nc -l` capture showed it).
- scripts/mac-tunnel-smoke.sh already used a 0600 header file.
- Grep found no other `-H "...Bearer $..."` in docs/, scripts/, README,
  CLAUDE.md or AGENTS.md.

## #3557: atomic token write without following symlinks
The remote half now works like this:
- `mktemp "$dir/.api-token.XXXXXXXX"` (exclusive, 0600) under umask 077.
- An EXIT trap removes the temp file; HUP/INT/TERM exit through it.
- It refuses a symlinked or foreign-owned directory, and a real directory at
  `api-token`.
- A symlink at `api-token` is unlinked, never followed.
- Then it does an atomic `mv` and checks the result is a regular file.
- `DICTATE_MAC_SSH` lets the tests run the remote half locally.

## Tests
- **`scripts/test-mac-hub.sh`** is hermetic and was added to `just check-cpu`
  and `just test-mac-hub`. Result: 47/0. It also passes under
  `gawk --posix`, since Ubuntu CI uses mawk. It uses a stub ssh that runs the
  "remote" locally, a fake `sshd -G` and a fake lsof. Coverage:
  - every refusal path above, plus the macOS-shaped safe config passing;
  - the post-check: `*:P`, `[::]:P` and a LAN address are each killed with
    78; loopback v4/v6 stays up and SIGTERM stops it;
  - token: 2000 pre-planted `.api-token.<pid>` symlinks plus a symlinked
    `api-token`. The victim stays untouched; the pre-fix script DOES clobber
    it (checked by running the old script under the new test). A
    directory-symlink, a real directory, a symlinked dir, and no token in
    output or argv;
  - docs: the documented curl lines are run with a fake curl, and the test
    fails if the token reaches argv. The old doc line fails it.
- **`scripts/mac-tunnel-sshd-test.sh`** (`just mac-tunnel-sshd-test`, not in
  check-cpu because CI has no sshd). Result: 8/0. A real non-root sshd on
  127.0.0.1, with a temp config, keys, known_hosts and an ephemeral port:
  - a: `no` comes up loopback-only, traffic reaches the hub side, and
    SIGTERM closes it;
  - b: `yes` is refused before any forward;
  - c: `Match User me` + `yes` in an Include is refused, although sshd -G
    says `no`;
  - d: an unreadable Include is refused;
  - e: runs in `unshare -rn` + a nested user namespace mapping you to
    yourself. sshd really runs `GatewayPorts yes` behind a safe decoy
    config, really binds `*:P` (lsof saw `*:P *:P`), and the post-check
    kills it with 78. The wildcard exists only inside that private netns.
- **`scripts/mac-tunnel-smoke.sh --port 17313`**: PASS, using isolated
  release CPU builds of 57c69ca against mac-mini. Steps that passed:
  - a real preflight of the Mac;
  - the wrapper as a transient unit, with "listener verified
    loopback-only";
  - Mac lsof shows `127.0.0.1:17313` only;
  - the token is provisioned 0600/0700 and the hash matches;
  - status 200, 401 with no token or a wrong one, a transcript returned for
    the token, 401 for an unauthenticated upload;
  - SIGKILL brings it back re-verified;
  - stop closes the port;
  - cleanup.
- **`scripts/install-smoke.sh --bindir target/release`**: PASS. It now
  checks that the wrapper is installed and executable, that ExecStart points
  at it, `RestartPreventExitStatus=78`, the `-R 127.0.0.1:` form, and that
  uninstall removes the wrapper.
- `systemd-analyze --user verify` passes on the unit with ExecStart pointed
  at the repo wrapper. The repo unit itself names `%h/.local/bin/…`, which
  exists only after install.
- `cargo test -p dictated --lib doctor`: 7/0.
- **`just check-cpu`**: durable job aef96045 on 57c69ca succeeded (exit 0). cutover-test, test-mac-hub 47/0, cargo test 1154 passed / 0 failed, the three clippy lines, fmt and the cli tree check are all clean. Two earlier submissions (3dde41c8, c37aa4dd) failed `job_admission_timed_out` without running anything: harness friction #3575.

## Untouched
The live dictated, `~/.config/dictate-agent/config.toml`, the hub firewall and
the Mac's sshd config: the Mac was only read (`sshd -G`, cat, lsof). The real
tunnel unit was never installed, and no real token was printed or read.

## Install steps after landing (unchanged order)
1. `just install`.
2. Merge `config/mac-hub-api.toml`, then `dictated --check-config`.
3. `dictated --api-token >/dev/null`, then restart dictated and run
   `dictate doctor`.
4. `just install-mac-tunnel`. This now also installs
   `~/.local/bin/dictate-mac-tunnel`. Check
   `journalctl --user -u dictate-mac-tunnel`: you should see "preflight ok"
   and "tunnel up … (verified loopback only)".
5. `just mac-token`.
6. Run the `curl -K -` check from docs/MAC-APP.md.
