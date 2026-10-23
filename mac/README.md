# DictateHub (macOS client)

A menu-bar dictation app for the Mac. Hold the hotkey, speak, and release.
The clip goes to the hub's S33 network API (`POST /v1/transcribe`), and the
cleaned-up text is pasted at the cursor. All inference runs on the hub; the
app holds no model. Plan: Issue #3527. Slices: M1 (#3529) MVP, M2 (#3543)
stable signing, permissions window, install + login item, feedback, file mode.

## Layout

```
Package.swift               SwiftPM, Swift 6 language mode, arm64
Sources/DictateHubCore/     testable logic, no microphone or event taps
  WAV.swift                 Float32 → Int16, 16 kHz mono WAV encoder
  HubClient.swift           request builder, transcript/status/ProtoError parsing,
                            HubError → menu text, HTTP(S) URLSession transport (30 s)
  UnixSocketTransport.swift Network.framework UDS HTTP/1.1, deadline, bounded parser
  Hotkey.swift              hotkey choices (right-hand device bits), edge detector,
                            hold-to-talk state machine (250 ms minimum, Esc, 120 s cap)
  Settings.swift            UserDefaults settings; 0600 token file
  PasteboardSnapshot.swift  snapshot/restore every pasteboard item and type
  AppPhase.swift            idle / recording / transcribing / error → icon, frames, line
  ClipAccumulator.swift     clip buffer + idle pre-roll ring (warm microphone)
  AudioFileLoader.swift     any audio file → 16 kHz mono Float32 (file mode)
  LaunchOptions.swift       --smoke / --transcribe-file / --unregister-login-item;
                            the file-mode DICTATEHUB_RESULT line
  Onboarding.swift          permission states, launch checklist, Privacy pane links
  PermissionDiagnosis.swift  which permissions are missing + the on-screen message with
                            its System Settings paths; the AeroSpace alt-n check
Sources/DictateHub/         the AppKit shell: status item, AVAudioEngine capture
                            (cold or warm), NSEvent hotkey monitor, Cmd+V via CGEvent,
                            Permissions and Settings windows, sounds + error bubble,
                            SMAppService login item
Tests/DictateHubCoreTests/  Swift Testing, including an NWListener stub hub
Resources/Info.plist        bundle template (LSUIElement, mic usage string)
scripts/remote.sh           run from the hub: rsync to the Mac + every command below
scripts/bundle.sh           on the Mac: release build → build/DictateHub.app, codesign
scripts/signing-setup.sh    on the Mac: dedicated signing keychain + identity (idempotent)
scripts/install.sh          on the Mac: → /Applications/DictateHub.app, migrate ~/Applications, launch once
scripts/lib-launchservices.sh lsregister helpers (register/unregister bundles) + test-only overrides
scripts/clean-stale.sh      on the Mac: unregister + delete stale build bundles (keeps the current id)
scripts/ls-unregister-build.sh on the Mac: unregister this build dir's bundle from LaunchServices
scripts/lib-signing.sh      shared ad-hoc guard (install, bundle, remote)
scripts/test-install-guard.sh shell tests for the guard and launch hygiene (remote.sh test)
scripts/uninstall.sh        on the Mac: login item + app (--purge: token, settings, logs)
scripts/probe.sh            on the Mac: bundled app's Test connection against an nc stub
scripts/file-probe.sh       on the Mac: file mode against a stub hub (synthetic `say` clip)
```

## Build, test, install (from the hub)

```bash
mac/scripts/remote.sh test        # swift test + shell guard tests on the Mac
mac/scripts/remote.sh build       # swift build
mac/scripts/remote.sh bundle      # build/DictateHub.app, signed (see Signing)
mac/scripts/remote.sh dr-check    # two builds → same designated requirement
mac/scripts/remote.sh run 4       # bundle, open it, smoke mode quits after 4 s
mac/scripts/remote.sh probe       # bundled app → stub /v1/status over plain HTTP
mac/scripts/remote.sh file-probe  # bundled app file mode → stub /v1/transcribe
mac/scripts/remote.sh install [--allow-adhoc]
                                  # bundle → /Applications/DictateHub.app, launch,
                                  # Open at login on (first install), clean-stale
mac/scripts/remote.sh uninstall [--purge] [--remove-signing-keychain]
mac/scripts/remote.sh clean-stale [--dry-run]
                                  # unregister + delete old build bundles under
                                  # ~/dictate-mac-build/ except this build id's
mac/scripts/remote.sh clean            # delete this build dir on the Mac
```

The package is synced to `mac-mini:~/dictate-mac-build/<id>.noindex/`. The id is
`$DICTATE_MAC_BUILD_ID`, else `$RSI_SESSION_ID`, else `manual`; the host is
`$DICTATE_MAC_HOST`. Only `install` (and `signing-setup`, run by `bundle`; `clean-stale`, inside `~/dictate-mac-build/`)
writes outside that dir. Install needs no sudo. It never installs an ad-hoc bundle silently: `remote.sh
install` sets `DICTATEHUB_REQUIRE_SIGNED=1`, so `bundle.sh` fails instead of
falling back to ad hoc, and `install.sh` refuses an ad-hoc bundle too. Pass
`--allow-adhoc` to override both. The reason: an ad-hoc signature's
requirement is its cdhash, so a grant made then is pinned to that one build
and every later build is "not trusted" while System Settings still shows the
switch on. A running copy is
stopped and replaced, and the new copy is launched. Its stderr goes to
$1
## Install location, migration, stale bundles (#3807)

- **Destination: `/Applications/DictateHub.app`**, owned by the console user. `/Applications` is
  group-writable for an admin user, so there is no sudo. Spotlight, Launchpad and Finder find it there;
  `install.sh` also runs `lsregister -f` on it. A bundle that is not quarantined opens with a plain
  `open` even though Gatekeeper (`spctl`) rejects the self-signed identity; `install.sh` clears any
  quarantine flag and never downloads the app.
- **Migration from `~/Applications`.** `install.sh` removes an old per-user copy: it runs that copy's
  `--unregister-login-item` (through `open`), stops it, `lsregister -u`s it and deletes it. If the login
  item was on, `install.sh` resets the first-install flag so the new copy turns it on from
  `/Applications`; if the user had switched it off, it stays off. At launch the app also re-registers
  the login item once when it runs from `/Applications`, the item is on, and the path it last
  registered from differs (`LoginItemPlan`); smoke, file-mode and build-dir runs never touch it.
  Privacy grants follow the designated requirement, which is the same at the new path.
- **Opening the running app shows something.** `applicationShouldHandleReopen` (Finder, Launchpad,
  Spotlight, `open`) opens the **Permissions** window while onboarding is not done or any permission is
  missing, otherwise **Settings**, and activates the app (`ReopenWindow`).
- **Stale build bundles are never launchable.** The build dir is `~/dictate-mac-build/<id>.noindex/`
  (Spotlight skips `.noindex` directories; `.metadata_never_index` is dropped in the root and the dir
  too). `remote.sh test|run <secs>|probe|file-probe|dr-check|stop|clean` unregister this build's bundle
  from LaunchServices afterwards. `remote.sh clean-stale` (also run at the end of `install`) unregisters
  and deletes every `DictateHub.app` with this bundle id under `~/dictate-mac-build/` except the one in
  the current build id's dir; it never touches `/Applications` or `~/Applications`, and `--dry-run`
  lists what it would remove.
- Test hooks: `DICTATEHUB_DEST_DIR`, `DICTATEHUB_LEGACY_DEST`, `DICTATEHUB_LSREGISTER`,
  `DICTATEHUB_OPEN`, `DICTATEHUB_DEFAULTS`, `DICTATEHUB_BUILD_ROOT` (see `lib-launchservices.sh`) let
  `test-install-guard.sh` run the install and cleanup against a stub HOME and destination. Nothing else
  sets them.

## Signing (stable privacy grants, no operator step)

macOS keys the Microphone and Accessibility grants to the app's designated
requirement (DR). An ad-hoc signature's DR is its cdhash, which changes on
every build. The login keychain cannot sign over SSH: it is locked for the
SSH security session, and codesign fails with `errSecInternalComponent`
(#3541). So `bundle.sh` signs with a dedicated identity:

- `scripts/signing-setup.sh` runs on first use and is idempotent. It creates
  `~/Library/Keychains/dictatehub-signing.keychain-db` with a random
  password in `dictatehub-signing.password` beside it (0600, never in the
  repo). It adds a self-signed "DictateHub Local Signing" code-signing
  certificate (RSA 2048, EKU codeSigning, 20 years) and sets the key's
  partition list, so codesign needs no prompt.
- `bundle.sh` unlocks that keychain non-interactively. The password goes to
  `security -i` on stdin, never in argv. It then signs with
  `codesign --keychain`, and the DR becomes
  `identifier "dev.jakedevar.DictateHub" and certificate leaf = H"<sha1>"`:
  the same on every build, so the grants survive rebuilds and reinstalls
  (`remote.sh dr-check` proves it).
- The keychain is **not** added to the search list. codesign does not need
  that with `--keychain`, and a locked keychain in the search list can make
  other apps show unlock prompts. The login keychain, the default keychain
  and the search list stay unchanged, and the certificate is not marked
  trusted (that would need an admin prompt). Neither codesign nor TCC needs
  trust.
- `signing-setup.sh --reset` makes a **new** certificate, so the grants must
  then be given again. `DICTATEHUB_SIGN_IDENTITY=<sha1>` signs with another
  identity, such as Apple Development (needs the login keychain unlocked).
  `DICTATEHUB_ADHOC=1` signs ad hoc.

## First run: what the operator clicks

`remote.sh install` launches the app, and its **DictateHub Permissions**
window opens. It also reopens at launch while anything below is missing, and
the menu item **Permissions…** opens it any time. Each row's status updates
every second.

1. **Hub token.** On the hub run `just mac-token` (#3528). Or paste the
   output of `dictated --api-token` into the token field and click **Save**.
2. **Microphone.** Click **Allow…**, then **Allow** in the macOS dialog. If it
   was denied earlier, the button reads **Open Settings…**. In that pane,
   switch DictateHub on.
3. **Accessibility.** Click **Open Accessibility Settings…**. In
   System Settings → Privacy & Security → Accessibility, switch **DictateHub**
   on (macOS asks for the login password or Touch ID). The row turns green
   within a second, and the hotkey starts working with no relaunch.
4. **Hotkey test.** Click into another app (TextEdit), hold **Right Option**,
   say a few words, and release. The row turns green when the key is seen,
   and the text is pasted. If Accessibility is on but the row stays grey,
   click **Input Monitoring…** and switch DictateHub on there too.
5. **Hub connection.** Click **Test connection**. A working tunnel shows
   `Hub OK: dictated …`.
6. Click **Done**.

Permissions are re-read, with no relaunch, when the app becomes active (for
example coming back from System Settings), every second while the Permissions
window is open (every 3 s in the background), and on **Re-check permissions**
in that window and in the menu. A permission that is missing is named on
screen, in the Permissions window ("What is missing") and in the menu-bar
error state (red triangle, the menu lists each one), each with its path:

- Accessibility: System Settings → Privacy & Security → Accessibility
- Input Monitoring: System Settings → Privacy & Security → Input Monitoring
  (needed by the Option+N event tap; hold-to-talk keys need Accessibility only)
- Microphone: System Settings → Privacy & Security → Microphone

For Accessibility and Input Monitoring the text adds: if DictateHub already
looks enabled there, remove it with − and add `/Applications/DictateHub.app`
again with + (a grant made for an older build does not carry over). With
Option+N chosen and AeroSpace binding `alt-n` in
`~/.config/aerospace/aerospace.toml` (read only, never edited), the window
also warns that AeroSpace may take the key first.

### Launch hygiene

The app is only ever started through LaunchServices, so macOS attributes it
(and its TCC grants) to DictateHub itself. A binary executed over SSH is
attributed to sshd. Every launch path uses `open`: `remote.sh run`, `probe`,
`file-probe`, `transcribe-file` (file mode, `open -W -n -g`), `install.sh`
(`open`) and `uninstall.sh` (`open -W`). `scripts/test-install-guard.sh` fails
if any script runs `Contents/MacOS/DictateHub` directly.

Grants persist across `remote.sh install` / rebuilds because of the stable
DR. To revoke them, use System Settings → Privacy & Security (select
DictateHub, then "−"). The scripts never touch TCC.

## Runtime

- Endpoint: UserDefaults `hubEndpoint`, default `tunnel` (**Hub (secure tunnel)**), connecting
  to `~/Library/Application Support/DictateHub/run/hub.sock` over SSH. Old
  default loopback URLs migrate automatically; custom HTTP(S) URLs stay.
  `unix:/absolute/path` selects a custom socket for isolated checks. Hotkey: UserDefaults `hotkey`
  (`right_option` | `fn` | `right_command` | `control_option` | `option_n`).
  The first four are hold-to-talk; `option_n` (Option+N, only Option held) toggles: press to start,
  press again to send, Esc to cancel. It uses an active event tap that swallows the chord (and Esc
  while recording) so no tilde or `n` reaches the focused app; if the tap or Accessibility is
  missing the menu-bar status says so. The default stays `right_option`.
- Token: `~/Library/Application Support/DictateHub/api-token` (0600, directory
  0700).
- Settings… → **Start and stop sounds** (default on). These are quiet system
  sounds: Tink plays when the microphone is live, Pop on release, and Basso
  on an error.
- Errors are the only pop-up. The icon turns into a triangle, Basso plays,
  and a small bubble under the menu-bar icon shows the reason for 5 s. No
  notification permission is needed.
- While busy the icon animates: mic.fill↔mic while recording, and
  ellipsis.circle↔fill while transcribing.
- Settings… → **Keep the microphone ready between dictations** (default
  **off**):
  - Off: the microphone opens at key-down. The start sound plays when audio
    is actually flowing, so it means "speak now". The log records
    `capture live N ms after key-down (cold start)`.
  - On: the engine runs while idle, and every clip starts with the last
    0.3 s before key-down, so a word spoken at key-down is kept. The cost:
    the orange microphone indicator stays on, and a Bluetooth headset
    (AirPods) stays in its low-quality call profile while DictateHub runs.
  - The warm engine never starts before Microphone access is granted, so it
    cannot raise the prompt.
- Settings… → **Open DictateHub at login** (SMAppService). The first launch
  of the installed copy turns it on. After that the system's state is
  respected, so switching it off in System Settings → General → Login Items
  is never undone. Build-dir and smoke launches never register.
- A hotkey press while a DictateHub window is frontmost does not dictate
  (there is nowhere to paste). That is why the hotkey test asks you to hold
  the key in another app.
- One instance: a second normal launch quits and leaves the running one.
- Logs: unified log, subsystem `dev.jakedevar.DictateHub`, plus stderr.
  Transcript text and the token are never logged.

## File mode (end-to-end check)

```bash
# From the hub, against the REAL hub through the tunnel (installed app,
# saved endpoint and token):
mac/scripts/remote.sh transcribe-file clip.wav             # print the text
mac/scripts/remote.sh transcribe-file clip.wav --insert    # also paste it at
                                                           # the frontmost app's cursor
# On the Mac directly:
/Applications/DictateHub.app/Contents/MacOS/DictateHub --transcribe-file clip.wav [--insert] [--app NAME]
```

The app decodes any audio file to 16 kHz mono and runs it through the same
WAV encoder, `HubClient` and (with `--insert`) the pasteboard swap and Cmd+V
as a live dictation. It prints one line,
`DICTATEHUB_RESULT {"ok":…,"text":…,"hub_ms":…,"audio_s":…,"inserted":…,"app":…}`,
and exits 0 on success or 1 on failure. `--app` sets `?app=`. The default is
the frontmost app with `--insert`, otherwise none.

`--insert` needs the Accessibility grant, and that grant belongs to the app,
so launch it through LaunchServices (`open -g -W`, as `remote.sh` does). A
binary started straight from an SSH shell is attributed to sshd instead.
`say -o clip.wav --data-format=LEI16@16000 "…"` makes a synthetic clip.
`--endpoint URL` and `--token-dir DIR` override the saved settings in file
and smoke mode only (`file-probe.sh` uses them with a stub).
