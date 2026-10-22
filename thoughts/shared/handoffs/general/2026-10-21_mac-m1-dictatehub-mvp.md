# Mac M1: DictateHub menu-bar MVP (Issue #3529, plan #3527)

Worker 596a14a2 (Claude, claude-opus-5-5). Base master 8231cde. Code commit
695574a9af660fe5a67ef25d196ac619c23ff16f. Everything lives under `mac/`, plus
a one-line pointer in CLAUDE.md "Workspace layout". There are no Rust or
Python changes.

## What was built

SwiftPM package `DictateHub` (Swift 6 language mode, arm64). `mac/README.md`
has the layout and the commands.

- **DictateHubCore** (headless-testable):
  - the WAV encoder (Float32 → Int16, clamped, symmetric ×32767, NaN → 0)
  - `HubRequestBuilder` and `HubClient`
  - transcript, status and ProtoError decoding
  - the `HubError` → menu-text mapping
  - the hotkey choices plus the edge detector and the `HoldToTalk` state machine
  - `HubSettings` (UserDefaults) and `TokenStore` (0600 file)
  - `PasteboardSnapshot` and `PasteboardSwap`
  - `AppPhase`
- **DictateHub** (the AppKit shell):
  - an `NSStatusItem` whose SF Symbol shows the state (mic / mic.fill / ellipsis.circle / exclamationmark.triangle)
  - the menu: status line, hotkey hint, Test connection, Settings…, Open Accessibility/Microphone Settings… (shown only when needed), Quit
  - `AVAudioEngine` capture through `AVAudioConverter` to 16 kHz mono
  - NSEvent global and local monitors
  - Cmd+V via `CGEvent`
  - a SwiftUI Settings window (endpoint, hotkey, token, Test connection)

## Decisions (and why)

- **Hotkey detection uses NSEvent global monitors, not a CGEventTap.** They
  need only Accessibility, which paste needs anyway, so the user grants one
  permission instead of two. The cost: the monitor is passive, so Esc still
  reaches the frontmost app, and the hotkey modifier is not swallowed.
  Right-hand keys are detected from the IOKit device bits
  (`NX_DEVICERALTKEYMASK` 0x40, `NX_DEVICERCMDKEYMASK` 0x10). Fn is detected
  from the device-independent `.function` bit, and Ctrl+Option from
  control && option on either side.
- **Press/release semantics.** The press captures the frontmost app name and
  starts capture. A release before 250 ms discards the clip silently. Esc
  cancels, and the following release is swallowed. At 120 s the clip is
  sent automatically and the real release is swallowed. Presses during
  recording or transcribing are ignored, because the hub has one session
  slot anyway.
- **Each clip gets a fresh AVAudioEngine.** A device change between clips
  needs no observers. The cost is engine start latency, which can clip the
  first ~100–200 ms of speech. M2 may want a warm engine or a pre-roll ring.
- **The restore is skipped if the pasteboard changed.** A copy the user
  makes within the 300 ms window is never clobbered. The transient text is
  also marked `org.nspasteboard.TransientType` so clipboard managers skip it.
- **CGEvent flags are set to exactly `.maskCommand`.** A still-held Control
  (in Ctrl+Option mode) therefore cannot turn the paste into Ctrl+Cmd+V.
- **Query escaping allows RFC 3986 unreserved characters only.** `&`, `=`,
  `+`, `#` and spaces in an app name always arrive intact. This is tested
  against URLComponents decoding.
- **The timeout is 30 s on both the request and the whole resource.** A
  stalled tunnel cannot hang the app. Status probes use 10 s.
- **The token is file-only in M1, with no Keychain.** The issue made the
  Keychain optional. The file is written as a 0600 temp file and renamed
  into place, so it is never briefly readable. A file found with a looser
  mode is tightened on load.
- **No TCC prompts at launch.** `AXIsProcessTrustedWithOptions(prompt: false)`
  runs at launch and every 3 s; once trust appears, the monitors are rearmed.
  The microphone prompt is requested on the first hold. In smoke mode the
  Mac's TCC state was not changed.
- **Minimum OS is macOS 15.** The APIs used are all ≤ macOS 14, and the
  target Mac runs 27.0.1, so there is no reason to require 27.
- **No hardened runtime.** A Development-signed local app does not need
  notarization. M2 can add `--options runtime` with
  `com.apple.security.device.audio-input` if it wants distribution.
- **Smoke-only flags `--smoke N [--probe] [--endpoint] [--token-dir]`.**
  They let a headless check run the bundled app against a stub without
  touching the user's UserDefaults or token, and they are ignored outside
  smoke mode.

## Results on the Mac (macOS 27.0.1, Xcode 27.1, Swift 6.4, arm64)

- `swift build`: clean, 0 warnings (from a fresh `.build`).
- `swift test`: **50 tests in 10 suites passed, 0 failed**, 0 warnings. The
  suites are WAV, HubRequest, ResponseParsing, StubServer, HotkeyChoice,
  HoldToTalk, AppPhase, TokenStore, Settings and Pasteboard. The stub-server
  suite uses an NWListener on an ephemeral 127.0.0.1 port and covers:
  - a transcribe round trip, checking method, path+query, Content-Type,
    Bearer, `Host: 127.0.0.1:<port>`, no `Origin`, and that the body equals
    the WAV
  - a status round trip
  - 401 unauthorized
  - 409 busy with `retry_after_ms`
  - a timeout (0.5 s against a 3 s stub)
  - a closed port mapping to unreachable
  - an empty clip refused before sending

  The pasteboard tests use `NSPasteboard.withUniqueName()`, never the general
  pasteboard.
- `remote.sh run 4`: the bundled app launched via `open` and logged
  `status item installed=true visible=true`, then quit itself after 4 s.
  `remote.sh stop` then reported "not running".
- `remote.sh probe`: the bundled app's Test connection reached an `nc` stub
  at `127.0.0.1:<random>` over plain HTTP (so ATS does not block the
  loopback IP). The stub saw `GET /v1/status` with the Bearer token, and the
  app parsed the answer: `Hub OK: dictated 0.0.0-stub · stub-model (cpu)`.

## Signing outcome

The Apple Development identity is present (`security find-identity` lists 1
valid identity). Signing with it over SSH **failed with
`errSecInternalComponent`**: the login keychain is locked for the SSH
security session, and `security show-keychain-info` reports "User
interaction is not allowed". `bundle.sh` fell back to **ad-hoc** signing
(`Signature=adhoc`, DR = cdhash), and the result verifies under
`codesign --verify --strict`. Ad hoc means TCC grants do not survive a
rebuild. Exact unlock step, on the Mac:

```
security unlock-keychain ~/Library/Keychains/login.keychain-db      # login password
security set-key-partition-list -S apple-tool:,apple: -s ~/Library/Keychains/login.keychain-db   # once
```

The other route is to run `mac/scripts/bundle.sh` from Terminal on the Mac.
`bundle.sh` picks the identity by SHA-1 (the name contains the account email)
and redacts the Authority line in its output.

## Left for M2

- Human-in-the-loop end-to-end on the Mac: grant Accessibility and
  Microphone, hold Right Option in TextEdit, speak, release, and check the
  text at the cursor and the clipboard restored. Test each hotkey choice,
  including Fn/Globe: the system setting "Press 🌐 key to" may also open the
  emoji picker or switch the input source, so M2 should check it or advise
  "Do Nothing". Test Esc cancel and the 120 s cap. Run against the real hub
  through the tunnel (#3528).
- Identity signing, which needs the keychain step above. Until then every
  rebuild invalidates the TCC grants.
- Onboarding window (Microphone, Accessibility, and Input Monitoring if it
  turns out to be needed), login item, `/Applications` install script, and
  the operator's install/use note.
- Optional: Keychain storage for the token, a warm audio engine or pre-roll
  to avoid clipping the first word, earcons, and a level meter. A non-loopback
  `http://` endpoint (for example Tailscale MagicDNS) would be blocked by ATS
  unless it is https or an ATS exception is added.
