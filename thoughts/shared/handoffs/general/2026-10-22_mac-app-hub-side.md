# Mac app hub side (#3528, plan #3527)

Worker 7b700dc9, 2026-10-22. Sandbox branch `rsi/7b700dc9-7ae2-49f7-8323-f9611826eb18`,
base master `8231cde`. Not pushed, per the manager brief: the PM integrates.

## Commits

- `6ce2c9a` fix(dictated): a network API that cannot start never takes dictation down
- `1d78aea` feat(mac): loopback API block, reverse SSH tunnel unit, token provisioning
- `827af5b` fix(dictated): logs go to stderr so --api-token's stdout is only the token
- (this file) docs(thoughts): handoff

## What changed

| Deliverable | Where |
|---|---|
| Tunnel unit | `systemd/dictate-mac-tunnel.service`: `ssh -N -T -R 127.0.0.1:${DICTATE_MAC_PORT}:127.0.0.1:${DICTATE_HUB_API_PORT} ${DICTATE_MAC_HOST}`, BatchMode, StrictHostKeyChecking=yes, ExitOnForwardFailure, ServerAlive 15x3, ConnectTimeout 15, ControlPath=none, Restart=always, RestartSec=10, StartLimitIntervalSec=0. Defaults mac-mini / 7313 / 7313 via `Environment=`; override with a drop-in. |
| Recipes | `just`/`make install-mac-tunnel` (install, daemon-reload, `enable --now`; under DESTDIR it only writes the file), `uninstall-mac-tunnel`, `just mac-token`, `just mac-tunnel-smoke`. `install-files` also ships `docs/MAC-APP.md` and `mac-hub-api.toml`. |
| Token script | `scripts/mac-provision-token.sh [--host H] [--remote-dir D]`. Runs `dictated --api-token`, keeps only a whole `dct1_` line, sends the token on ssh **stdin** (on no argv, not on stdout), and writes it under umask 077 to a temp file that is renamed into `~/Library/Application Support/DictateHub/api-token` (0600, dir 0700). It checks the SHA-256 and modes and prints only the path. |
| API block | `config/mac-hub-api.toml` (enabled, `127.0.0.1:7313`, no LAN, no origins, no raw text, `allowed_hosts = []`). The test `the_mac_hub_api_example_is_loopback_only_and_loads_clean` pins it: no warnings, `bind_plan().loopback`. `config.example.toml` points at it. |
| Robustness | `Daemon::serve_network_api` replaces the fatal `start_network_api` call in `run_with_report`. Any `StartError` is logged as a warning, binds nothing, and records `ApiStatus::Down(reason)` in an `ApiStatusCell` shared with the doctor. Test: `a_taken_port_or_a_refused_api_leaves_the_daemon_serving` (port squatted, then token missing → Down; socket status + transcribe still work; the API comes up once the port is free). |
| Doctor | `network_api` (ok listening + TCP connect probe / FAIL "enabled but DOWN: reason" / warn "enabled but not started" / skipped when disabled) and `mac_tunnel` (`systemctl --user show` of the unit: ok running, warn otherwise, skipped when the unit is not installed or the API is off). Unit-tested verdicts and parser. |
| Docs | `docs/MAC-APP.md` (setup, security model, drop-ins, troubleshooting, removal). An amendment note in the S33 design doc §4. `CLAUDE.md` and the `serve.rs` doc comment updated. |
| Found defect, fixed | `dictated` logged through tracing to **stdout**, so `--api-token` printed a "Config loaded" line before the token, breaking its "pipe-able" promise. Tracing now writes to stderr. |

## Decisions (and why)

- **Only listener-start failures degrade.** Rules 1–6 of the S33 design (LAN without opt-in, wildcard bind, half TLS) remain config validation errors and still stop `dictated` before anything starts. Rule 7 (token), EADDRINUSE and unusable TLS files now degrade, fail-closed. Reason: the issue requires that the API never take dictation down, and failing closed never increases exposure.
- **No `dictate status` change.** The API state is shown in `doctor` only, which avoids a wire-protocol change (additive-only rule, golden tests). The issue said "status/doctor".
- **Doctor tunnel check is a warning at worst.** The tunnel's absence never affects hub dictation, and it is skipped when the unit is not installed.
- **Equal ports.** The Host policy accepts `127.0.0.1:<bound port>`. Different Mac/hub ports would get a 421 unless `allowed_hosts` lists the Mac spelling. The docs say so.
- **Not pushed** (manager brief + worker contract say do not push; the binding text asks for a push). The brief is the more specific instruction, and the sandbox shares the object store with the PM's checkout.

## Open risks

- **Stale Mac-side forward.** If the hub drops off the network without a FIN, the Mac's sshd can hold 127.0.0.1:7313 until TCP gives up. `ExitOnForwardFailure` plus `Restart=` keeps retrying, but the Mac app sees "hub unreachable" until then. Optional mitigation on the Mac: `ClientAliveInterval 30` (an operator choice; not applied).
- **Config typo stops dictated.** An invalid `[api]` block (rules 1–6) is a config error, as before. The PM's enable step must run `dictated --check-config` before the restart.
- **The `mac_tunnel` doctor probe shells out to `systemctl --user`** from the daemon. It is bounded at 5 s and runs only when `[api]` is enabled.
- **CUDA clippy / `just e2e`** were not run (integrator's job). The diff touches no dictate-stt, cuda or transcription code.

## Verification

- `systemd-analyze --user verify` passes on the repo unit and on the `make install-mac-tunnel DESTDIR=…`-rewritten unit.
- SSH from a systemd user context: `systemd-run --user --wait --pipe ssh -o BatchMode=yes mac-mini …` works. The key has no passphrase and no agent is needed; the host key is in known_hosts. Mac sshd config sets no `GatewayPorts`, so the default `no` applies and forwards bind loopback.
- `scripts/mac-tunnel-smoke.sh` PASS (log `/tmp/3528-smoke.log`), against isolated release CPU builds from this branch on port 17313:
  - daemon A API on loopback only (`ss`);
  - daemon B with A's port: socket answers, logs "network API is DOWN", doctor shows `Network API … DOWN`, local `dictate transcribe` works;
  - the unit file's own ExecStart, run as transient `systemd-run --user` unit `dmt-smoke-<pid>` with ports overridden: Mac `lsof` shows only `127.0.0.1:17313`;
  - `mac-provision-token.sh --remote-dir /tmp/dictatehub-smoke.*` wrote 0600 / 0700 and the hash matched;
  - from the Mac: `GET /v1/status` with token → `type=status`; no token → 401; wrong token → 401; `POST /v1/transcribe?app=smoke` short.wav → `type=transcript`, `route=type`, text non-empty; unauthenticated POST → 401;
  - `systemctl kill -s KILL` of the tunnel → Restart brought it back, status 200;
  - stop → Mac port closed; afterwards no daemons, transient units or temp dirs on either host.
- `scripts/install-smoke.sh --bindir target/release --variant cpu` PASS (it now covers install/uninstall-mac-tunnel under DESTDIR).
- Gate: `just check-cpu` durable job b0cd9513 green (1154 passed / 0 failed) on `1d78aea` (+ the main.rs edit landing mid-run). **Final gate on `827af5b`: `just check-cpu` job 76c74a71 green, 1154 passed / 0 failed / 0 ignored**, with clippy -D warnings on 3 lines, fmt and the dictate-cli tree check all clean. With that build, `target/debug/dictated --api-token` (temp XDG dirs) prints exactly one line on stdout, the `dct1_` token.
- The live `dictated` (pid 1514, started 12:40) was never touched. `~/.config/dictate-agent/config.toml` was not modified, and the real tunnel unit was never installed.
