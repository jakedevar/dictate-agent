# DictateHub (macOS client)

A menu-bar dictation app for the Mac. Hold the hotkey, speak, and release.
The clip goes to the hub's S33 network API (`POST /v1/transcribe`), and the
cleaned-up text is pasted at the cursor. All inference runs on the hub; the
app holds no model. Plan: Issue #3527. This is slice M1 (#3529).

## Layout

```
Package.swift               SwiftPM, Swift 6 language mode, arm64
Sources/DictateHubCore/     testable logic, no microphone or event taps
  WAV.swift                 Float32 → Int16, 16 kHz mono WAV encoder
  HubClient.swift           request builder, transcript/status/ProtoError parsing,
                            HubError → menu text, URLSession transport (30 s)
  Hotkey.swift              hotkey choices (right-hand device bits), edge detector,
                            hold-to-talk state machine (250 ms minimum, Esc, 120 s cap)
  Settings.swift            UserDefaults settings; 0600 token file
  PasteboardSnapshot.swift  snapshot/restore every pasteboard item and type
  AppPhase.swift            idle / recording / transcribing / error → icon + line
Sources/DictateHub/         the AppKit shell: status item, AVAudioEngine capture,
                            NSEvent hotkey monitor, Cmd+V via CGEvent, Settings window
Tests/DictateHubCoreTests/  Swift Testing, including an NWListener stub hub
Resources/Info.plist        bundle template (LSUIElement, mic usage string)
scripts/remote.sh           run from the hub: rsync to the Mac + build/test/bundle/run/probe
scripts/bundle.sh           run on the Mac: release build → build/DictateHub.app, codesign
scripts/probe.sh            run on the Mac: bundled app's Test connection against an nc stub
```

## Build, test, bundle, run (from the hub)

```bash
mac/scripts/remote.sh test        # swift test on the Mac
mac/scripts/remote.sh build       # swift build
mac/scripts/remote.sh bundle      # build/DictateHub.app, signed (see below)
mac/scripts/remote.sh run 4       # bundle, open it, smoke mode quits after 4 s
mac/scripts/remote.sh run         # bundle and leave it running; then `stop`
mac/scripts/remote.sh probe       # bundled app → stub /v1/status over plain HTTP
mac/scripts/remote.sh clean       # delete this build dir on the Mac
```

The package is synced to `mac-mini:~/dictate-mac-build/<id>/`. The id is
`$DICTATE_MAC_BUILD_ID`, else `$RSI_SESSION_ID`, else `manual`; the host is
`$DICTATE_MAC_HOST`. Nothing is installed, and the `.app` stays in the
build dir.

## Signing

`bundle.sh` signs with the first "Apple Development" identity it finds, so
the designated requirement (bundle id plus certificate) stays the same and
the Microphone and Accessibility grants survive rebuilds. Over SSH the login
keychain is locked for the SSH security session, and codesign fails with
`errSecInternalComponent`. The script then signs ad hoc, which works but
changes the cdhash on every build, so TCC grants do not carry over. To sign
with the identity over SSH:

```bash
ssh mac-mini
security unlock-keychain ~/Library/Keychains/login.keychain-db   # login password
# once, so codesign may use the key without a GUI prompt:
security set-key-partition-list -S apple-tool:,apple: -s ~/Library/Keychains/login.keychain-db
```

Or run `scripts/bundle.sh` from Terminal on the Mac itself, where the keychain
is unlocked.

## Runtime

- Endpoint: UserDefaults `hubEndpoint`, default `http://127.0.0.1:7313` (the
  hub API through the reverse SSH tunnel). Hotkey: UserDefaults `hotkey`
  (`right_option` | `fn` | `right_command` | `control_option`).
- Token: `~/Library/Application Support/DictateHub/api-token` (0600, directory
  0700). Paste it in Settings, or copy it from the hub's `dictated --api-token`.
- Permissions: Accessibility (global hotkey monitor and Cmd+V) and Microphone.
  The app never prompts at launch. "Open Accessibility Settings…" in the menu
  registers the app and opens the pane, and the microphone prompt appears on
  the first hold.
- Logs: unified log, subsystem `dev.jakedevar.DictateHub`, plus stderr.
  Transcript text and the token are never logged.
- `--smoke N [--probe] [--endpoint URL] [--token-dir DIR]`: launch-check mode.
  The app quits after N s, and the overrides are honored only in this mode.
