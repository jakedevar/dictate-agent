# Mac M2: DictateHub signing, permissions, install, feedback, file mode (Issue #3543, plan #3527)

Worker f7a87262 (Claude, claude-opus-5-5). Base master 968e283 (M1, #3529).
Closes #3541. Code commits: 4bf3b47 (app) and e3c4f6e (scripts and README).
Everything is under `mac/`. There are no Rust or Python changes.

## What was built

1. **Stable signing with no operator step** (`scripts/signing-setup.sh`, `bundle.sh`).
   - A dedicated keychain `~/Library/Keychains/dictatehub-signing.keychain-db`.
   - Its random password sits beside it in `dictatehub-signing.password`
     (0600, never in the repo).
   - A self-signed "DictateHub Local Signing" certificate: RSA 2048, EKU
     codeSigning, 20 years.
   - `set-key-partition-list` so codesign needs no prompt.
   - `bundle.sh` unlocks the keychain non-interactively and runs
     `codesign --keychain`.
   - The resulting designated requirement (DR) is
     `identifier "dev.jakedevar.DictateHub" and certificate leaf = H"27a5d983…"`.
2. **Permissions window.** It opens on the first run, and at every launch
   while something is missing; it is also menu → Permissions…. Each row's
   status is polled every 1 s:
   - Microphone
   - Accessibility
   - Hotkey test, with an Input Monitoring fallback button
   - Hub token: paste field plus a `just mac-token` hint
   - Hub connection: Test connection
3. **Install and login item.**
   - `remote.sh install` installs to `~/Applications/DictateHub.app` with no
     sudo and refuses an ad-hoc bundle.
   - The first launch of the installed copy registers
     `SMAppService.mainApp`. A Settings toggle controls it afterwards.
   - `remote.sh uninstall [--purge] [--remove-signing-keychain]` removes it.
   - Only one instance runs at a time.
4. **Audio start.**
   - Cold capture by default. The start sound plays when the first audio
     buffer arrives ("speak now"), and the key-down → live latency is logged.
   - Opt-in **warm microphone** toggle with a 300 ms pre-roll ring
     (`ClipAccumulator`).
5. **Feedback.**
   - Tink/Pop/Basso system sounds at volume 0.35, with a toggle.
   - The menu-bar icon animates while recording and transcribing.
   - Errors only: a 5 s popover bubble under the icon.
6. **File mode.** `--transcribe-file WAV [--insert] [--app NAME]` sends the
   file through the same encoder, `HubClient` and paste path as a live
   dictation, and prints one `DICTATEHUB_RESULT {json}` line. It exits 0 on
   success, 1 on failure and 2 on bad arguments.
   `remote.sh transcribe-file WAV [--insert]` is the PM's end-to-end helper.

## Decisions (and why)

- **The signing keychain is not added to the search list.** This deviates
  from the Issue text on purpose.
  - Tested on macOS 27.0.1 over SSH: `codesign --keychain <kc>` finds the
    identity without it.
  - A locked keychain in the search list can make other apps (Safari, Mail)
    show unlock prompts on the operator's screen.
  - The login keychain, the default keychain and the search list are all
    unchanged. Checked before and after: `security list-keychains -d user`
    and `security default-keychain` both show only login.keychain-db.
- **The certificate is not trusted.** `add-trusted-cert` needs an
  authorization prompt, and neither codesign nor TCC needs trust: the DR is
  the leaf hash.
- **Passwords go through `security -i` on stdin**, never in argv, so `ps`
  cannot see them.
- **The keychain auto-locks** after 10 minutes and on sleep. `bundle.sh`
  unlocks it each time. There is no explicit lock after signing, so two
  concurrent bundles cannot lock each other out.
- **The warm mic is off by default.**
  - The Mac's only input device is Bluetooth AirPods
    (`system_profiler SPAudioDataType`). An always-open mic would hold them
    in the low-quality call profile and keep the orange indicator lit.
  - Cold start instead makes the start sound mean "live now", so the user
    starts speaking after the cue.
  - The latency could not be measured: the app has no Microphone grant,
    and I may not click the prompt. The app logs
    `capture live N ms after key-down (cold start)`, so the first real run
    gives the number. If it is long with AirPods, turn on Settings →
    "Keep the microphone ready".
- **Errors use a popover, not UserNotifications.** Banners would need a
  third permission prompt. The icon, the Basso sound and a 5 s bubble under
  the icon need none.
- **Input Monitoring is not verified.** It is unknown whether a global
  NSEvent `flagsChanged` monitor needs it on macOS 27. That cannot be
  checked without a human key press and the grants. So the hotkey-test row
  shows whether the *global* monitor saw the key, and offers an Input
  Monitoring button only when Accessibility is on and the key was not seen.
- **A press while our own window is frontmost no longer dictates** (M1 did).
  There is nowhere to paste, and it would race the token field.
- **The login item is registered once.** A `loginItemConfigured` flag means
  a user's "off" in System Settings is never undone at the next launch.
  `uninstall.sh` clears the flag, so a reinstall turns it on again. Build-dir
  and smoke launches never register.
- **File mode decodes and re-encodes** (AVAudioFile → 16 kHz mono float →
  `WAV.encode`) instead of uploading the bytes as is. That is the live
  path's exact encoder.
  - In file mode the activation policy is `.prohibited`, and `remote.sh`
    uses `open -g -W`, so Cmd+V reaches the frontmost app.
  - The file is launched via LaunchServices so the Accessibility grant
    belongs to the app, not to sshd.

## Results on the Mac (macOS 27.0.1, Xcode 27.1, Swift 6.4, arm64)

- `swift build` from a clean `.build`: 0 warnings. The release build is clean.
- `remote.sh test`: **72 tests in 15 suites passed, 0 failed**: M1's 50
  plus 22 new. The new suites are ClipAccumulator, LaunchOptions,
  AudioFileLoader, M2Settings and Onboarding.
- `remote.sh signing-setup`, run twice: the first run creates the keychain
  and identity; the second only verifies them.
- `remote.sh dr-check`:
  ```
  build 1: CDHash=1267fd93c37c86ec68851691402fe1eb3012fa21
           designated => identifier "dev.jakedevar.DictateHub" and certificate leaf = H"27a5d983614cda3e895c9b6dee80ed2ad16d098c"
  build 2: CDHash=c62b2f45ff22ae0380e7021d14d08019642f8e3f
           designated => identifier "dev.jakedevar.DictateHub" and certificate leaf = H"27a5d983614cda3e895c9b6dee80ed2ad16d098c"
  DR STABLE: same certificate-based designated requirement across two different builds
  ```
  `bundle.sh` prints `SIGNED=identity`, which meets #3541's acceptance.
- `remote.sh run 4` → SMOKE OK.
- `remote.sh probe` → PROBE OK.
- `remote.sh file-probe` → FILE PROBE OK. The clip was synthetic (`say`,
  2.92 s, 46672 samples). The stub saw `POST /v1/transcribe?app=FileProbe`
  with Bearer and `audio/wav`, and a 16 kHz mono 16-bit body holding all
  46672 samples. The app printed the stub's text with `"ok":true,"inserted":false`.
- File-mode error paths:
  - closed port → `Hub unreachable…`, exit 1
  - missing file → exit 1
  - no token → exit 1
  - missing value or conflicting flags → exit 2
- Install lifecycle:
  - install → `login item registered`, `login item: enabled`, running
  - reinstall over the running copy → stopped and replaced, still enabled
  - a second `open -n` → "another DictateHub is running; quitting"
  - uninstall → `login item: enabled -> off`, app and process gone
  - install again → registered and enabled again
  - The window list shows the 500×459 Permissions window on screen.
- **Final state on the Mac:**
  - `~/Applications/DictateHub.app` is installed and running, with Open at
    login enabled and the Permissions window open.
  - No token and no grants yet.
  - The app has never contacted the live hub.

## Left for the end-to-end step (PM / operator)

- The operator clicks: see `mac/README.md` "First run". In short:
  - token: `just mac-token`
  - Microphone: Allow… → Allow
  - Accessibility: Open Accessibility Settings… → switch DictateHub on
  - Hotkey test: in TextEdit hold Right Option, speak, release
  - Test connection
- Once the tunnel is up:
  - `mac/scripts/remote.sh transcribe-file <synthetic.wav>`: text only.
  - Then the same with `--insert` and TextEdit frontmost.
- Read `capture live N ms after key-down` from
  `~/Library/Logs/DictateHub/launch.log` or the unified log to settle
  cold vs warm for AirPods.
- Find out whether Input Monitoring is needed: the hotkey row stays grey
  with Accessibility on.
- Carried over from M1:
  - Fn/Globe: the "Press 🌐 key to" system setting may need "Do Nothing".
  - Esc cancel and the 120 s cap.
- The TCC grants persist across reinstalls, since the DR is stable. Do not
  run `signing-setup.sh --reset` unless you mean to re-grant.
