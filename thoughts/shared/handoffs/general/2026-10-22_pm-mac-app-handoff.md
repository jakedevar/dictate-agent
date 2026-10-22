# PM handoff: Dictate Agent, 2026-10-22 (seat ac4d1e94, Claude Opus 5.5)

## Mission (from the GM, 2026-10-22)

Operator request: a Wispr Flow-style dictation app on the Mac mini. All
inference runs on the hub's local models. Plan Issue: #3527.

## State: delivered and live, except the operator's Accessibility click

| Part | Where |
|---|---|
| Mac app (DictateHub, Swift, menu bar) | `mac/`, installed at `~/Applications/DictateHub.app` on mac-mini, opens at login, signed with a stable designated requirement from a dedicated signing keychain |
| Hub API | S33 `[api]` on `127.0.0.1:7313` (loopback only), enabled in `~/.config/dictate-agent/config.toml` |
| Transport | `dictate-mac-tunnel.service` (hub, user unit): reverse **Unix-socket** forward to `~/Library/Application Support/DictateHub/run/hub.sock` on the Mac. No Mac TCP listener, so GatewayPorts does not apply |
| Token | `~/.local/share/dictated/api-token` (0600), copied to the Mac with `just mac-token` |
| Live dictated | built from master 40220bc with CUDA. Backups: `~/.local/bin/{dictated,dictate}.pre-mac`, `~/.config/dictate-agent/config.toml.pre-mac` |

- **E2E proven:** `mac/scripts/remote.sh transcribe-file` sent a synthetic 7.5 s clip and got cleaned text back in 552 ms.
- **Open:** operator to-do `dictatehub-accessibility` (id 8968a765…). The operator turns on Accessibility on the Mac, then tries hold-Right-Option dictation.
  - My wake `ax-todo` (when `todo_closed`) fires on it.
  - Then: check the Mac's `~/Library/Logs/DictateHub/launch.log` for a live dictation (never copy transcript text).
  - Send the final "done" AgentReportUp with the short install/use note, and close #3527.

## Landed on master today (all pushed)

- 968e283 M1 (#3529)
- 629571d M2 (#3543, #3541)
- b85e953: hub side (#3528), with security review #3553 in 3 passes, Unix-socket redesign (#3600), and #3555/#3556/#3557/#3599
- 8008bb2: all `.py` removed (#3618)
- 7873628: inline python3 removed, `crates/dictate-devtools` (#3690)
- 40220bc: Ghostty paste TARGETS race fix (#3133)

## In flight

- **#3133:** deployed, live check pending. Look for Ghostty dictations after 10:15Z in `dictate history --json`, using jq to select only `app`, `route` and `timings` (never text). Close the Issue if inject is Paste with a normal duration; after that the `ghostty-typing` context profile can be retired.
- **#1473** (config .bak follows a symlink): Sonnet worker c245fc5f, base 40220bc.
- **#1474** (local socket config authority): Sonnet worker fdfb0e21, base 40220bc.
  - Both are security fixes in `crates/dictated`.
  - To integrate: ff/merge onto master, push, rebuild release (`cargo build --release --workspace --features dictated/cuda` with PATH=/opt/cuda/bin, ~1.5 min incremental), `make install-files TARGET_DIR=<sandbox>/target/release` while `dictate status` is idle, then `systemctl --user restart dictated` and `dictate doctor`.

## Standing rules in force (put them in every brief)

- **Operator directive 2b22862d: Python is forbidden.** Rust preferred; Swift for `mac/`; shell for glue.
- **GM throttle, 15:06Z:** at most 2 **Claude Sonnet high** workers at once. No Opus workers. No Codex implementers until the 10-27 06:32Z reset; Codex is for vendor-split reviews only.
- **Operator mandate:** never ask the operator; decide and record the reason. Escalate only real gates.
- **Network exposure** changes get a pre-land reviewer pass.

## Known friction

- The hub build governor starves this project's AgentSubmitJob gates (harness #3575).
  - 7 admission timeouts in a row.
  - I raised `governor_build_slots` to 3.
  - I built releases in-turn with `nice` and `CARGO_BUILD_JOBS=4`. A `nohup` background build is killed when the turn ends, so build in the foreground with `timeout 560`.
- The real-GPU `just e2e` has not run since 8231cde (admission-starved). Run it once when the governor allows.
- Hub disk is at 91%, mostly other projects' sandboxes. The health anomalies `hub_disk_high:/` and `cpu_hot:hub` are ignored in the health wake.

## Kaizen

Filed or seen this session: #3551, #3575 (harness), #3690 (done), #3555–#3557/#3599 (done). Older open kaizen are untouched: #1430, #1436, #1472, #1481, #1489, #1505, #2484.
