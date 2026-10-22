import AppKit
import AVFoundation
import DictateHubCore

/// Owns the status item and runs one dictation at a time:
/// press → record, release → upload → paste, Esc → cancel.
///
/// The same `deliver(samples:app:insert:)` path serves a live dictation and
/// file mode (`--transcribe-file`), so the end-to-end check exercises the
/// real encoder, hub client and paste.
@MainActor
final class AppController: NSObject, NSApplicationDelegate {
    let options: LaunchOptions
    private let settings = HubSettings()
    private let tokens: TokenStore
    private let recorder = AudioRecorder()
    private var hold = HoldToTalk()
    private var toggle = ToggleToTalk()
    private let hotkeys: HotkeyMonitor
    private var statusItem: NSStatusItem?
    private var settingsModel: SettingsModel?
    private var settingsWindow: SettingsWindowController?
    private var onboarding: OnboardingWindowController?
    private lazy var feedback = Feedback(soundsEnabled: { [settings] in settings.playSounds })
    private var trustTimer: Timer?
    private var animationTimer: Timer?
    private var animationFrame = 0
    private var trusted = false
    private var micDenied = false
    /// Name of the app that was frontmost when the hotkey went down; sent as
    /// `?app=` so the hub applies that app's profile.
    private var targetApp: String?
    /// Last connection-test answer, shown under the status line.
    private var connectionLine: String?

    private var endpoint: String { options.endpointOverride ?? settings.endpoint }

    private(set) var phase: AppPhase = .idle {
        didSet {
            if phase != oldValue { animationFrame = 0 }
            refresh()
        }
    }

    init(options: LaunchOptions) {
        self.options = options
        hotkeys = HotkeyMonitor(choice: HubSettings().hotkey)
        tokens = options.tokenDirectory.map { TokenStore(directory: URL(fileURLWithPath: $0)) } ?? TokenStore()
        super.init()
    }

    // MARK: lifecycle

    func applicationDidFinishLaunching(_ notification: Notification) {
        switch options.mode {
        case .transcribeFile(let path, let insert, let app):
            runFileMode(path: path, insert: insert, app: app)
            return
        case .normal:
            if let other = otherInstance() {
                Log.info("another DictateHub is running (pid \(other.processIdentifier)); quitting")
                other.activate()
                NSApp.terminate(nil)
                return
            }
        case .smoke, .unregisterLoginItem:
            break
        }

        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        item.button?.imagePosition = .imageOnly
        statusItem = item

        hotkeys.onPress = { [weak self] global in self?.hotkeyPressed(global: global) }
        hotkeys.onRelease = { [weak self] in self?.hotkeyReleased() }
        hotkeys.onEscape = { [weak self] in self?.escapePressed() }
        hotkeys.isRecording = { [weak self] in self?.phase == .recording }

        checkTrust()
        // Accessibility can be granted while we run; global monitors only
        // start delivering after they are (re)installed, so poll and rearm.
        trustTimer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.checkTrust() }
        }
        recorder.keepWarm = settings.keepMicWarm
        refresh()
        let checklist = currentChecklist()
        Log.info("started: status item installed=\(item.button != nil) visible=\(item.isVisible) "
            + "hotkey=\(hotkeys.choice.rawValue) (\(hotkeys.choice.mode)) endpoint=\(endpoint) "
            + "token=\(checklist.tokenPresent ? "present" : "missing") accessibility=\(trusted) "
            + "microphone=\(checklist.microphone) warm=\(recorder.isWarm) sounds=\(settings.playSounds)")

        switch options.mode {
        case .smoke(let seconds, let probe, let showOnboarding):
            Log.info("smoke mode: quitting in \(seconds) s")
            if probe { testConnection() }
            if showOnboarding { openPermissions() }
            DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
                Log.info("smoke mode: quitting")
                NSApp.terminate(nil)
            }
        case .normal:
            configureLoginItemOnce()
            if checklist.shouldOpenAtLaunch(firstRun: !settings.onboardingDone) {
                openPermissions()
            }
        case .transcribeFile, .unregisterLoginItem:
            break
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        recorder.shutdown()
        hotkeys.stop()
    }

    private func otherInstance() -> NSRunningApplication? {
        guard let id = Bundle.main.bundleIdentifier else { return nil }
        let me = ProcessInfo.processInfo.processIdentifier
        return NSRunningApplication.runningApplications(withBundleIdentifier: id)
            .first { $0.processIdentifier != me && !$0.isTerminated }
    }

    /// The first launch of the installed copy turns "Open at login" on.
    /// Afterwards the system's state is the truth (a user who turned it off
    /// in System Settings is never overridden).
    private func configureLoginItemOnce() {
        guard LoginItem.isInstalledLocation else {
            Log.info("login item: skipped (not the installed copy: \(Bundle.main.bundlePath))")
            return
        }
        if !settings.loginItemConfigured {
            do {
                try LoginItem.set(true)
                Log.info("login item registered")
            } catch {
                Log.error("login item register: \(error.localizedDescription)")
            }
            settings.loginItemConfigured = true
        }
        Log.info("login item: \(LoginItem.statusText)")
    }

    private func currentChecklist() -> OnboardingChecklist {
        OnboardingChecklist(microphone: OnboardingModel.microphoneState(),
                            accessibility: OnboardingModel.accessibilityState(),
                            tokenPresent: tokens.load() != nil)
    }

    /// Shown in the menu bar when a chord hotkey's event tap is missing.
    private var tapMissingMessage: String {
        "\(hotkeys.choice.displayName) is off: the keyboard event tap is unavailable "
            + "(allow Accessibility and Input Monitoring)"
    }

    private func checkTrust() {
        let now = TextInserter.isTrusted()
        // A chord hotkey whose tap was lost or never created is retried here.
        let tapLost = trusted && now && hotkeys.needsTap && !hotkeys.tapActive
        guard now != trusted || statusItem?.menu == nil || tapLost else { return }
        trusted = now
        if trusted {
            hotkeys.start()
            if hotkeys.needsTap && !hotkeys.tapActive {
                showTapMissing()
            } else if case .error = phase {
                phase = .idle
            } else {
                refresh()
            }
        } else {
            hotkeys.stop()
            phase = .error("Accessibility not granted: hotkey and paste are off")
        }
    }

    private func showTapMissing() {
        let error = AppPhase.error(tapMissingMessage)
        if phase != error {
            Log.error("hotkey \(hotkeys.choice.rawValue): event tap not active")
            phase = error
        }
    }

    /// The hotkey choice changed: drop a toggle dictation that is running,
    /// then show (or clear) the missing-tap status for the new choice.
    private func hotkeyChoiceChanged(_ choice: HotkeyChoice) {
        if toggle.isCapturing { handle(toggle.escape()) }
        hotkeys.choice = choice
        if trusted && hotkeys.needsTap && !hotkeys.tapActive {
            showTapMissing()
        } else if case .error(let message) = phase, message.hasSuffix("(allow Accessibility and Input Monitoring)") {
            phase = .idle
        }
        refresh()
    }

    /// Microphone or Accessibility changed (seen by the permissions window).
    private func permissionsChanged() {
        checkTrust()
        recorder.reconcileWarm()
        if AVCaptureDevice.authorizationStatus(for: .audio) == .authorized { micDenied = false }
        refresh()
    }

    // MARK: dictation

    private func hotkeyPressed(global: Bool) {
        if global { onboarding?.model.hotkeySeen() }
        // Toggle mode: a press while recording is the stop.
        if hotkeys.choice.mode == .toggle, phase == .recording {
            handle(toggle.press())
            return
        }
        // Our own window is frontmost (Settings, Permissions): nothing to
        // paste into, so a press there does not start a dictation.
        if !global { return }
        switch phase {
        case .recording, .transcribing: return  // one dictation at a time
        case .idle, .error: break
        }
        guard trusted else { return }
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized:
            micDenied = false
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .audio) { granted in
                Task { @MainActor in
                    self.micDenied = !granted
                    self.phase = granted ? .idle : .error("Microphone access denied")
                    self.recorder.reconcileWarm()
                }
            }
            phase = .error("Allow microphone access, then hold the hotkey again")
            return
        default:
            micDenied = true
            fail("Microphone access denied")
            return
        }
        let keyDown = ProcessInfo.processInfo.systemUptime
        let started = hotkeys.choice.mode == .toggle
            ? toggle.press() == .start
            : hold.press(at: keyDown) == .start
        guard started else { return }
        targetApp = NSWorkspace.shared.frontmostApplication?.localizedName
        do {
            try recorder.start(
                keyDown: keyDown,
                onReady: { [weak self] latencyMs, preRollMs in
                    guard let self, self.recorder.isRecording else { return }
                    Log.info(preRollMs > 0
                        ? "capture live with \(preRollMs) ms pre-roll (warm)"
                        : "capture live \(latencyMs) ms after key-down (cold start)")
                    self.feedback.started()
                },
                onLimit: { [weak self] in self?.captureLimitReached() })
            phase = .recording
        } catch {
            _ = hold.release(at: ProcessInfo.processInfo.systemUptime)
            toggle.reset()
            let message = (error as? RecorderError)?.message ?? "Microphone failed to start"
            Log.error("capture start: \(error)")
            fail(message)
        }
    }

    private func hotkeyReleased() {
        handle(hold.release(at: ProcessInfo.processInfo.systemUptime))
    }

    private func escapePressed() {
        handle(hold.escape() ?? toggle.escape())
    }

    private func captureLimitReached() {
        Log.info("capture reached \(Int(AudioRecorder.maxSeconds)) s cap; sending")
        handle(hold.limitReached() ?? toggle.limitReached())
    }

    private func handle(_ action: HoldToTalk.Action?) {
        guard let action else { return }
        guard recorder.isRecording else {
            toggle.reset()  // the recorder is already stopped: no stale toggle state
            return
        }
        let samples = recorder.stop()
        switch action {
        case .start:
            return
        case .discard, .cancel:
            Log.info(action == .cancel ? "cancelled" : "tap under minimum hold; discarded")
            phase = .idle
        case .finish:
            feedback.stopped()
            phase = .transcribing
            let app = targetApp
            Task { [weak self] in
                guard let self else { return }
                switch await self.deliver(samples: samples, app: app, insert: true) {
                case .delivered:
                    self.phase = .idle
                case .failed(let message):
                    self.fail(message)
                }
            }
        }
    }

    /// An error from a dictation: icon, sound, and a bubble under the icon.
    private func fail(_ message: String) {
        phase = .error(message)
        feedback.failed()
        feedback.showError(message, below: statusItem?.button)
    }

    enum Delivery {
        case delivered(text: String, hubMs: Int, inserted: Bool)
        case failed(String)
    }

    /// Encode → POST /v1/transcribe → (optionally) paste at the cursor.
    private func deliver(samples: [Float], app: String?, insert: Bool) async -> Delivery {
        guard !samples.isEmpty else { return .failed(HubError.emptyAudio.userMessage) }
        let client: HubClient
        do {
            client = try makeClient()
        } catch {
            return .failed(hubError(fromTransport: error).userMessage)
        }
        let seconds = WAV.duration(sampleCount: samples.count)
        // Encode off the main actor: 120 s is ~3.8 MB.
        let wav = await Task.detached { WAV.encode(floatSamples: samples) }.value
        let started = Date()
        let transcript: Transcript
        do {
            transcript = try await client.transcribe(wav: wav, app: app)
        } catch {
            let e = hubError(fromTransport: error)
            Log.error("transcribe: \(e.userMessage)")
            return .failed(e.userMessage)
        }
        let ms = Int(Date().timeIntervalSince(started) * 1000)
        Log.info(String(format: "transcribed %.1f s clip in %d ms (%d chars)",
                        seconds, ms, transcript.text.count))
        guard insert, !transcript.text.isEmpty else {
            return .delivered(text: transcript.text, hubMs: ms, inserted: false)
        }
        guard TextInserter.isTrusted() else {
            return .failed("Accessibility not granted: cannot paste")
        }
        await TextInserter.insert(transcript.text)
        return .delivered(text: transcript.text, hubMs: ms, inserted: true)
    }

    private func makeClient() throws -> HubClient {
        guard let token = tokens.load() else { throw HubError.missingToken }
        return HubClient(builder: try HubRequestBuilder(endpoint: endpoint, token: token))
    }

    // MARK: file mode

    /// `--transcribe-file WAV [--insert] [--app NAME]`: one clip through the
    /// live path, one `DICTATEHUB_RESULT {json}` line on stdout, exit 0 on
    /// success. No status item, no hotkey, no microphone.
    private func runFileMode(path: String, insert: Bool, app: String?) {
        Task {
            var result = FileModeResult(ok: false)
            do {
                let url = URL(fileURLWithPath: path)
                let samples = try await Task.detached { try AudioFileLoader.load(url) }.value
                let target = app ?? (insert ? NSWorkspace.shared.frontmostApplication?.localizedName : nil)
                result.app = target
                result.audioSeconds = (WAV.duration(sampleCount: samples.count) * 100).rounded() / 100
                Log.info("file mode: \(samples.count) samples, insert=\(insert), endpoint=\(endpoint)")
                switch await deliver(samples: samples, app: target, insert: insert) {
                case .delivered(let text, let ms, let inserted):
                    result.ok = true
                    result.text = text
                    result.hubMs = ms
                    result.inserted = inserted
                case .failed(let message):
                    result.error = message
                }
            } catch let error as AudioFileError {
                result.error = error.message
            } catch {
                result.error = error.localizedDescription
            }
            print(result.line)
            fflush(stdout)
            exit(result.ok ? 0 : 1)
        }
    }

    // MARK: connection test

    @objc private func testConnection() {
        let client: HubClient
        do {
            client = try makeClient()
        } catch {
            showConnection("Test failed: " + hubError(fromTransport: error).userMessage)
            return
        }
        connectionLine = "Testing \(endpoint)…"
        refresh()
        Task { [weak self] in
            let line: String
            do {
                line = "Hub OK: " + (try await client.status()).summary
            } catch {
                line = "Test failed: " + hubError(fromTransport: error).userMessage
            }
            Log.info(line)
            self?.showConnection(line)
        }
    }

    private func showConnection(_ line: String) {
        connectionLine = line
        refresh()
        settingsWindow?.showResult(line)
        onboarding?.model.hubLine = line
    }

    // MARK: menu

    private func refresh() {
        guard let statusItem else { return }
        updateIcon()
        updateAnimation()
        if let button = statusItem.button {
            button.toolTip = "DictateHub: \(phase.statusLine(mode: hotkeys.choice.mode))"
        }

        let menu = NSMenu()
        menu.autoenablesItems = false
        let status = NSMenuItem(title: phase.statusLine(mode: hotkeys.choice.mode), action: nil, keyEquivalent: "")
        status.isEnabled = false
        menu.addItem(status)
        let hint = NSMenuItem(title: hotkeys.choice.hint, action: nil, keyEquivalent: "")
        hint.isEnabled = false
        menu.addItem(hint)
        if let connectionLine {
            let item = NSMenuItem(title: connectionLine, action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        }
        menu.addItem(.separator())
        menu.addItem(item("Test connection", #selector(testConnection)))
        menu.addItem(item("Permissions…", #selector(openPermissions)))
        menu.addItem(item("Settings…", #selector(openSettings), key: ","))
        if micDenied {
            menu.addItem(item("Open Microphone Settings…", #selector(openMicrophonePane)))
        }
        menu.addItem(.separator())
        menu.addItem(item("Quit DictateHub", #selector(quit), key: "q"))
        statusItem.menu = menu
    }

    private func updateIcon() {
        guard let button = statusItem?.button else { return }
        let frames = phase.animationFrames
        let name = frames[animationFrame % frames.count]
        let image = NSImage(systemSymbolName: name, accessibilityDescription: "DictateHub")
        image?.isTemplate = true
        button.image = image
    }

    /// Recording and transcribing cycle the icon; idle and error are still.
    private func updateAnimation() {
        let animate = phase.animationFrames.count > 1
        if animate && animationTimer == nil {
            animationTimer = Timer.scheduledTimer(withTimeInterval: AppPhase.frameInterval, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    self.animationFrame += 1
                    self.updateIcon()
                }
            }
        } else if !animate {
            animationTimer?.invalidate()
            animationTimer = nil
        }
    }

    private func item(_ title: String, _ action: Selector, key: String = "") -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        item.target = self
        return item
    }

    @objc private func openSettings() {
        if settingsWindow == nil {
            let model = SettingsModel(settings: settings, tokens: tokens)
            model.onHotkeyChange = { [weak self] choice in
                self?.hotkeyChoiceChanged(choice)
            }
            model.onTest = { [weak self] in self?.testConnection() }
            model.onKeepWarmChange = { [weak self] on in self?.recorder.keepWarm = on }
            model.onOpenPermissions = { [weak self] in self?.openPermissions() }
            settingsModel = model
            settingsWindow = SettingsWindowController(model: model)
        }
        settingsWindow?.show()
    }

    @objc private func openPermissions() {
        if onboarding == nil {
            let model = OnboardingModel(
                tokens: tokens, settings: settings,
                testConnection: { [weak self] in self?.testConnection() },
                permissionsChanged: { [weak self] in self?.permissionsChanged() })
            onboarding = OnboardingWindowController(model: model)
        }
        onboarding?.show()
        if options.isSmoke, let model = onboarding?.model {
            Log.info("permissions window: microphone=\(model.microphone) accessibility=\(model.accessibility) "
                + "inputMonitoring=\(model.inputMonitoringGranted) token=\(model.tokenPresent)")
        }
    }

    @objc private func openMicrophonePane() {
        NSWorkspace.shared.open(PrivacyPane.microphone.url)
    }

    @objc private func quit() {
        NSApp.terminate(nil)
    }
}
