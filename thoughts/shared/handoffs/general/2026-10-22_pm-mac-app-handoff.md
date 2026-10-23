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

## Update 17:45Z

- The operator granted Accessibility (to-do closed). The done report was sent up and #3527 is closed.
- No live Mac hotkey dictation has been observed yet. Watch the hub history for Mac app names arriving via the network API.
- #1473, #1475 and #1476 were already fixed and are closed. #1474 is fixed by bc0606b, merged at master 3068cbf and deployed.
- #3133 is still open until a real Ghostty dictation shows inject=Paste. No workers are running.

## Update 19:15Z (seat near rotation; read this first)

- **#3762 Option+N:** master 9e37035, from the Mac lane's format-patch, which was reviewed and passes 97/97 Swift tests. Installed with remote.sh, and the operator's pref is `hotkey=option_n`.
- **P1 operator bug:** the app shows Accessibility NOT granted, and its tracking Issue is **#3766**.
  - Root cause (PM, verified read-only): the system TCC row csreq is `cdhash H"8e2ca256…"`, from an ad-hoc build granted at 04:47Z. The installed DR is the stable `certificate leaf = H"27a5d983…"`, which never matches.
  - Operator to-do `dictatehub-option-n` (id 1389f351, P1) was updated with the steps: remove DictateHub with − and re-add it in Accessibility and Input Monitoring, relaunch, then Option+N.
  - My `optn-todo` wake fires when that to-do closes.
- **Worker 64c5e341** (Sonnet, on #3766): re-poll trust on activation, a Re-check button, a missing-permission diagnosis, launch only via `open`, refuse ad-hoc installs, and an optional AeroSpace `alt-n` warning. It must not run `remote.sh install`.
  - When it lands, the PM `git am`s or merges it to master, pushes, runs `DICTATE_MAC_BUILD_ID=pm-e2e mac/scripts/remote.sh test`, then `install`.
  - Confirm the launch line shows `accessibility=true` after the operator re-adds the grant.
- **Close-out:** when a live Option+N dictation is in hub history (check with jq on app/route/timings, never text), close #3762 and #3766. Then send ONE AgentReportUp to the GM; it relays to the Mac via the Rsi PM.
- **AeroSpace:** the Mac lane commented out AeroSpace's `alt-n` binding in ~/.config/aerospace/aerospace.toml on the Mac. Never edit it from here.

## Update 10-23 02:35Z (seat 4b68d1fa, Claude Opus 5.5; read this first)

The GM appointed this seat after ac4d1e94 was interrupted with no successor.

- **Wakes:** `health` (when health_anomaly; ignores `hub_disk_high:/` and `cpu_hot:hub`) and `optn-todo` (when todo_closed on `1389f351-3dae-43cb-bbce-938e4d4f72e6`, to-do `dictatehub-option-n`, 24 h timeout). The earlier seat's wakes did not carry over, so both were re-armed. Confirm them with AgentListWakes.
- **#3762 / #3766:** waiting on the operator's step: remove DictateHub with − in Accessibility and Input Monitoring, re-add it, reopen.
  - When the to-do closes, look in hub history for a Mac-origin row: `dictate history --json --limit 20 | jq -r '.items[] | [(.ts_ms/1000|todate), .app, .route, .timings.inject.status] | @tsv'`. Never print text.
  - If a row is there, close both Issues and send ONE AgentReportUp. If not, ask the GM to relay a check of the Mac launch.log for `accessibility=true`.
  - As of 02:30Z there is no Mac dictation in history. The newest rows are Ghostty at 10-22 06:08Z and textedit (the pm-e2e test) at 09:35Z.
- **Closed this seat:**
  - #3133: fixed, with isolated Ghostty evidence. Decision: keep the `ghostty-typing` profile.
  - #3551 and #3542: worker-contract fix at master 0b27a30. Workers send no timeout_minutes and do not push.
  - #1430, #1436, #1472, #1481, #1489, #1505: obsolete, or harness items.
  - #2484 and #3801: storage, acknowledged.
- **Still open:** #3762 and #3766 (operator step), plus backlog #1497, #1498 and #1502 (P3/P4 features, idle on purpose per the GM throttle). No workers are running.
- **Kaizen:** none filed this seat. Fixed: #3551, #3542. Open: none in this project (harness #3575 is still the governor starvation).

GM THROTTLE (verbatim): at most 2 live Sonnet high workers; no Opus workers; Codex for reviews only (gpt-6-luna); the hub is shared and warm, so prefer idling over low-value work. PM at most ~10 turns/h with batched `when` wakes. Brief: keep the work in Issues; decide non-gate questions yourself; escalate only real gates (main/releases, money, credentials, deleting user data) through the GM; report up with AgentReportUp only at milestones; rotate at the threshold your catalog states (write the handoff, commit it last, report its path and stop).

Friction: none

## Update 10-23 02:40Z (seat 4b68d1fa): #3807, the app could not be found or opened

- **Operator P1 (via the GM):** "can't launch the setup wizard / not installed".
  - Root cause: the app was installed and running, but in `~/Applications`, and it is LSUIElement with no reopen handler, so opening it showed nothing.
  - 7+ stale self-signed build bundles were registered with LaunchServices and Spotlight, so the wrong bundle could be opened and granted.
- **Fix:** master a86caad (worker 54cef8ea).
  - Installs to `/Applications`, migrating out of `~/Applications`.
  - Reopening shows the Permissions or Settings window.
  - The login item is re-registered from the new path.
  - Builds go to `~/dictate-mac-build/<id>.noindex` and are unregistered after use; `remote.sh clean-stale` purges old bundles.
- **Installed 02:35Z:** pid 86627, login item enabled. lsregister and mdfind list only `/Applications/DictateHub.app`.
- **Still waiting:** to-do `dictatehub-option-n` (rewritten: open from Launchpad, remove and re-add the grants for /Applications/DictateHub.app, Option+N).
  - The `optn-todo` wake fires on it. Then close #3762, #3766 and #3807 when a Mac dictation reaches the hub history, and AgentReportUp once.
- **Mac rule:** never run osascript, System Events or sfltool over SSH. A login-items query hung and may have shown an automation prompt.
- Wakes: `health` and `optn-todo`; the safety wake was cancelled.

Friction: none

## Update 10-23 03:27Z (seat 4b68d1fa)

- **Accessibility:** granted for /Applications/DictateHub.app. The TCC auth is 2 at 03:17Z, the csreq is the stable leaf 27a5d983, and the app logged `event tap installed for option_n`.
- **Input Monitoring:** NOT granted (no kTCCServiceListenEvent row).
- **To-do `dictatehub-option-n`:** the operator closed it.
- **New to-do:** P1 `dictatehub-input-monitoring` (id fb1eb8d5-f519-4d24-9949-b4deaea843f5). The `optn-todo` wake now watches it.
- **No Mac dictation yet:** dictated saw only one Test-connection status call (03:14Z).
- **On the first live dictation:** close #3762, #3766 and #3807, then report DONE up once.
- **Check commands (read-only):**
  - `ssh mac-mini 'sqlite3 "/Library/Application Support/com.apple.TCC/TCC.db" "select service,auth_value from access where client like \"%DictateHub%\""'`
  - `journalctl --user -u dictated | grep network` (never print text)

Friction: none

## Update 10-23 03:48Z (seat 4b68d1fa): #3824, a crash on every recording

- **Symptom:** the operator saw "no app, no icon". DictateHub had crashed, three times: 03:17Z, then 03:40Z twice.
- **Crash:** EXC_BREAKPOINT in the `AudioRecorder.startEngine` mic tap closure.
  - It is a Swift 6 main-actor isolation check, tripped on AVFAudio's realtime queue at the first buffer.
  - So every Option+N crashed the app. The tests never start the mic.
- **Fix:** master 399e22a (closures are `@Sendable`).
  - Mac tests: Swift 119, guard 59.
  - Reinstalled at 03:47Z: pid 77547, accessibility=true, microphone granted, tap installed.
  - The TCC ListenEvent row exists.
- **To-do:** `dictatehub-input-monitoring` (fb1eb8d5) is rewritten as "Option+N once in a text field". The `optn-todo` wake watches it, and the `optn-recheck` safety wake fires at about 04:18Z.
- **Close on the first live dictation:** #3762, #3766, #3807 and #3824, then report DONE once.
- **Open follow-up in the #3824 body:** a mic-path smoke or test (`remote.sh` has none). Not started: low priority, per the GM throttle.
- **Crash reports:** `~/Library/Logs/DiagnosticReports/DictateHub-*.ips` on the Mac. Read the faulting thread with `plutil -extract threads.N.frames`.

Friction: none

## Update 10-23 04:31Z (seat 4b68d1fa): DONE, now idle

- **Operator, via the GM:** "The dictation on the mac mini is working!"
- **Hub evidence:** 3 network-API dictations from DictateHub at 04:27:55Z, 04:28:04Z and 04:28:16Z, with hub totals of 206–393 ms.
- **Closed:** #3762, #3766, #3807 and #3824. To-do `dictatehub-input-monitoring` closed as done.
- **Kaizen filed:** #3831 (P3), a mic tap path test so a Swift 6 isolation trap cannot ship again. Not started: the GM said go idle after filing.
- **Open:** #3831, plus backlog #1497, #1498 and #1502. All idle on purpose; no workers running.
- **Wakes:** `health` only.
- **Live:** /Applications/DictateHub.app at master 399e22a, pid 77547.

GM THROTTLE (verbatim): at most 2 live Sonnet high workers; no Opus workers; Codex for reviews only (gpt-6-luna); the hub is shared and warm, so prefer idling over low-value work. PM at most ~10 turns/h with batched `when` wakes. Brief: keep the work in Issues; decide non-gate questions yourself; escalate only real gates (main/releases, money, credentials, deleting user data) through the GM; report up with AgentReportUp only at milestones; rotate at the threshold your catalog states (write the handoff, commit it last, report its path and stop).

Friction: #3831
