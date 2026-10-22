import AppKit
import AVFoundation
import DictateHubCore

/// Owns the status item and runs one dictation at a time:
/// press → record, release → upload → paste, Esc → cancel.
@MainActor
final class AppController: NSObject, NSApplicationDelegate {
    private let settings = HubSettings()
    private let tokens: TokenStore
    private let recorder = AudioRecorder()
    private var hold = HoldToTalk()
    private let hotkeys: HotkeyMonitor
    private var statusItem: NSStatusItem!
    private var settingsWindow: SettingsWindowController?
    private var trustTimer: Timer?
    private var trusted = false
    private var micDenied = false
    /// Name of the app that was frontmost when the hotkey went down; sent as
    /// `?app=` so the hub applies that app's profile.
    private var targetApp: String?
    /// Last connection-test answer, shown under the status line.
    private var connectionLine: String?
    private let smokeSeconds: Double?
    /// Smoke mode only: an endpoint used instead of the saved one (never
    /// persisted), and whether to run "Test connection" once at launch.
    private let endpointOverride: String?
    private let probeAtLaunch: Bool

    private var endpoint: String { endpointOverride ?? settings.endpoint }

    private(set) var phase: AppPhase = .idle {
        didSet { refresh() }
    }

    init(arguments: [String]) {
        hotkeys = HotkeyMonitor(choice: HubSettings().hotkey)
        func value(after flag: String) -> String? {
            guard let i = arguments.firstIndex(of: flag), i + 1 < arguments.count else { return nil }
            return arguments[i + 1]
        }
        if arguments.contains("--smoke") {
            // Headless launch check: quit after N seconds. The overrides below
            // exist so a check can point at a stub server with a throwaway
            // token; outside smoke mode they are ignored.
            smokeSeconds = value(after: "--smoke").flatMap(Double.init) ?? 3
            endpointOverride = value(after: "--endpoint")
            tokens = value(after: "--token-dir").map { TokenStore(directory: URL(fileURLWithPath: $0)) }
                ?? TokenStore()
            probeAtLaunch = arguments.contains("--probe")
        } else {
            smokeSeconds = nil
            endpointOverride = nil
            tokens = TokenStore()
            probeAtLaunch = false
        }
        super.init()
    }

    // MARK: lifecycle

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        statusItem.button?.imagePosition = .imageOnly

        hotkeys.onPress = { [weak self] in self?.hotkeyPressed() }
        hotkeys.onRelease = { [weak self] in self?.hotkeyReleased() }
        hotkeys.onEscape = { [weak self] in self?.escapePressed() }

        checkTrust()
        // Accessibility can be granted while we run; global monitors only
        // start delivering after they are (re)installed, so poll and rearm.
        trustTimer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.checkTrust() }
        }
        refresh()
        Log.info("started: status item installed=\(statusItem.button != nil) visible=\(statusItem.isVisible) "
            + "hotkey=\(hotkeys.choice.rawValue) endpoint=\(endpoint) "
            + "token=\(tokens.load() == nil ? "missing" : "present") accessibility=\(trusted)")

        if let seconds = smokeSeconds {
            Log.info("smoke mode: quitting in \(seconds) s")
            if probeAtLaunch { testConnection() }
            DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
                Log.info("smoke mode: quitting")
                NSApp.terminate(nil)
            }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        _ = recorder.stop()
        hotkeys.stop()
    }

    private func checkTrust() {
        let now = TextInserter.isTrusted()
        guard now != trusted || statusItem.menu == nil else { return }
        trusted = now
        if trusted {
            hotkeys.start()
            if case .error = phase { phase = .idle } else { refresh() }
        } else {
            hotkeys.stop()
            phase = .error("Accessibility not granted: hotkey and paste are off")
        }
    }

    // MARK: dictation

    private func hotkeyPressed() {
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
                }
            }
            phase = .error("Allow microphone access, then hold the hotkey again")
            return
        default:
            micDenied = true
            phase = .error("Microphone access denied")
            return
        }
        guard hold.press(at: ProcessInfo.processInfo.systemUptime) == .start else { return }
        targetApp = NSWorkspace.shared.frontmostApplication?.localizedName
        do {
            try recorder.start { [weak self] in self?.captureLimitReached() }
            phase = .recording
        } catch {
            _ = hold.release(at: ProcessInfo.processInfo.systemUptime)
            let message = (error as? RecorderError)?.message ?? "Microphone failed to start"
            Log.error("capture start: \(error)")
            phase = .error(message)
        }
    }

    private func hotkeyReleased() {
        handle(hold.release(at: ProcessInfo.processInfo.systemUptime))
    }

    private func escapePressed() {
        handle(hold.escape())
    }

    private func captureLimitReached() {
        Log.info("capture reached \(Int(AudioRecorder.maxSeconds)) s cap; sending")
        handle(hold.limitReached())
    }

    private func handle(_ action: HoldToTalk.Action?) {
        guard let action, recorder.isRecording else { return }
        let samples = recorder.stop()
        switch action {
        case .start:
            return
        case .discard, .cancel:
            Log.info(action == .cancel ? "cancelled" : "tap under minimum hold; discarded")
            phase = .idle
        case .finish:
            send(samples: samples, app: targetApp)
        }
    }

    private func send(samples: [Float], app: String?) {
        guard !samples.isEmpty else {
            phase = .error(HubError.emptyAudio.userMessage)
            return
        }
        let client: HubClient
        do {
            client = try makeClient()
        } catch {
            phase = .error(hubError(fromTransport: error).userMessage)
            return
        }
        phase = .transcribing
        let seconds = WAV.duration(sampleCount: samples.count)
        Task { [weak self] in
            // Encode off the main actor: 120 s is ~3.8 MB.
            let wav = await Task.detached { WAV.encode(floatSamples: samples) }.value
            let started = Date()
            do {
                let transcript = try await client.transcribe(wav: wav, app: app)
                let ms = Int(Date().timeIntervalSince(started) * 1000)
                Log.info(String(format: "transcribed %.1f s clip in %d ms (%d chars)",
                                seconds, ms, transcript.text.count))
                guard let self else { return }
                if !transcript.text.isEmpty {
                    if self.trusted {
                        await TextInserter.insert(transcript.text)
                    } else {
                        self.phase = .error("Accessibility not granted: cannot paste")
                        return
                    }
                }
                self.phase = .idle
            } catch {
                let e = hubError(fromTransport: error)
                Log.error("transcribe: \(e.userMessage)")
                self?.phase = .error(e.userMessage)
            }
        }
    }

    private func makeClient() throws -> HubClient {
        guard let token = tokens.load() else { throw HubError.missingToken }
        return HubClient(builder: try HubRequestBuilder(endpoint: endpoint, token: token))
    }

    // MARK: connection test

    @objc private func testConnection() {
        let client: HubClient
        do {
            client = try makeClient()
        } catch {
            connectionLine = "Test failed: " + hubError(fromTransport: error).userMessage
            refresh()
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
            self?.connectionLine = line
            self?.refresh()
            self?.settingsWindow?.showResult(line)
        }
    }

    // MARK: menu

    private func refresh() {
        guard let statusItem else { return }
        if let button = statusItem.button {
            let image = NSImage(systemSymbolName: phase.symbolName, accessibilityDescription: "DictateHub")
            image?.isTemplate = true
            button.image = image
            button.toolTip = "DictateHub: \(phase.statusLine)"
        }

        let menu = NSMenu()
        menu.autoenablesItems = false
        let status = NSMenuItem(title: phase.statusLine, action: nil, keyEquivalent: "")
        status.isEnabled = false
        menu.addItem(status)
        let hint = NSMenuItem(title: "Hold \(hotkeys.choice.displayName) to dictate", action: nil, keyEquivalent: "")
        hint.isEnabled = false
        menu.addItem(hint)
        if let connectionLine {
            let item = NSMenuItem(title: connectionLine, action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        }
        menu.addItem(.separator())
        menu.addItem(item("Test connection", #selector(testConnection)))
        menu.addItem(item("Settings…", #selector(openSettings), key: ","))
        if !trusted {
            menu.addItem(item("Open Accessibility Settings…", #selector(openAccessibilityPane)))
        }
        if micDenied {
            menu.addItem(item("Open Microphone Settings…", #selector(openMicrophonePane)))
        }
        menu.addItem(.separator())
        menu.addItem(item("Quit DictateHub", #selector(quit), key: "q"))
        statusItem.menu = menu
    }

    private func item(_ title: String, _ action: Selector, key: String = "") -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        item.target = self
        return item
    }

    @objc private func openSettings() {
        if settingsWindow == nil {
            settingsWindow = SettingsWindowController(
                settings: settings, tokens: tokens,
                onHotkeyChange: { [weak self] choice in
                    self?.hotkeys.choice = choice
                    self?.refresh()
                },
                onTest: { [weak self] in self?.testConnection() })
        }
        settingsWindow?.show()
    }

    @objc private func openAccessibilityPane() {
        // Registers the app in the list (without granting anything) and opens the pane.
        _ = TextInserter.isTrusted(prompt: true)
        openPrivacyPane("Privacy_Accessibility")
    }

    @objc private func openMicrophonePane() {
        openPrivacyPane("Privacy_Microphone")
    }

    private func openPrivacyPane(_ anchor: String) {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)") {
            NSWorkspace.shared.open(url)
        }
    }

    @objc private func quit() {
        NSApp.terminate(nil)
    }
}
