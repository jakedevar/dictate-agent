import AppKit
import AVFoundation
import CoreGraphics
import DictateHubCore
import SwiftUI

/// The first-run "Permissions" window (also menu → Permissions…): one row per
/// thing dictation needs, each with a live status polled every second while
/// the window is open. The app never grants anything itself and never raises
/// a prompt until the user presses that row's button.
@MainActor
final class OnboardingWindowController: NSObject, NSWindowDelegate {
    let model: OnboardingModel
    private var window: NSWindow?
    private var poll: Timer?

    init(model: OnboardingModel) {
        self.model = model
    }

    var isVisible: Bool { window?.isVisible ?? false }

    func show() {
        model.refresh()
        if window == nil {
            let w = NSWindow(contentViewController: NSHostingController(rootView: OnboardingView(model: model)))
            w.title = "DictateHub Permissions"
            w.styleMask = [.titled, .closable]
            w.isReleasedWhenClosed = false
            w.delegate = self
            w.center()
            window = w
        }
        model.onClose = { [weak self] in self?.window?.close() }
        NSApp.activate()
        window?.makeKeyAndOrderFront(nil)
        startPolling()
    }

    func windowWillClose(_ notification: Notification) {
        poll?.invalidate()
        poll = nil
        model.closed()
    }

    private func startPolling() {
        poll?.invalidate()
        poll = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.model.refresh() }
        }
    }
}

@MainActor
@Observable
final class OnboardingModel {
    var microphone: PermissionState = .notAsked
    var accessibility: PermissionState = .notAsked
    var inputMonitoringGranted = false
    var tokenPresent = false
    var tokenInput = ""
    var tokenNote: String?
    var hubLine: String?
    var hotkeyName = ""
    var hotkeyMode: HotkeyMode = .hold
    /// When the hotkey was last seen by the global monitor (another app frontmost).
    var lastGlobalPress: Date?
    var now = Date()
    /// The user asked to open the Accessibility pane at least once.
    var accessibilityRequested = false

    private let tokens: TokenStore
    private let settings: HubSettings
    private let testConnection: () -> Void
    private let permissionsChanged: () -> Void
    var onClose: () -> Void = {}

    init(tokens: TokenStore, settings: HubSettings,
         testConnection: @escaping () -> Void, permissionsChanged: @escaping () -> Void) {
        self.tokens = tokens
        self.settings = settings
        self.testConnection = testConnection
        self.permissionsChanged = permissionsChanged
    }

    var checklist: OnboardingChecklist {
        OnboardingChecklist(microphone: microphone, accessibility: accessibility, tokenPresent: tokenPresent)
    }

    static func microphoneState() -> PermissionState {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: return .granted
        case .notDetermined: return .notAsked
        default: return .denied
        }
    }

    static func accessibilityState() -> PermissionState {
        TextInserter.isTrusted() ? .granted : .notAsked
    }

    func refresh() {
        let before = (microphone, accessibility)
        microphone = Self.microphoneState()
        accessibility = Self.accessibilityState()
        inputMonitoringGranted = CGPreflightListenEventAccess()
        tokenPresent = tokens.load() != nil
        hotkeyName = settings.hotkey.displayName
        hotkeyMode = settings.hotkey.mode
        now = Date()
        if before != (microphone, accessibility) { permissionsChanged() }
    }

    func requestMicrophone() {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .audio) { _ in
                Task { @MainActor in self.refresh() }
            }
        default:
            NSWorkspace.shared.open(PrivacyPane.microphone.url)
        }
    }

    func openAccessibility() {
        // Adds DictateHub to the list (switched off) and opens the pane; the
        // user turns the switch on. Nothing is granted here.
        _ = TextInserter.isTrusted(prompt: true)
        accessibilityRequested = true
        NSWorkspace.shared.open(PrivacyPane.accessibility.url)
    }

    func openInputMonitoring() {
        _ = CGRequestListenEventAccess()
        NSWorkspace.shared.open(PrivacyPane.inputMonitoring.url)
    }

    func saveToken() {
        let trimmed = tokenInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        do {
            try tokens.save(trimmed)
            tokenInput = ""
            tokenNote = "Saved (0600)"
        } catch {
            tokenNote = "Could not save: \(error.localizedDescription)"
        }
        refresh()
    }

    func test() {
        hubLine = "Testing…"
        testConnection()
    }

    func hotkeySeen() {
        lastGlobalPress = Date()
    }

    func done() {
        settings.onboardingDone = true
        onClose()
    }

    func closed() {
        // Closing the window counts as "seen": it reopens at launch only
        // while something is still missing.
        settings.onboardingDone = true
    }
}

struct OnboardingView: View {
    @Bindable var model: OnboardingModel

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("DictateHub needs two permissions and your hub's token. Nothing is granted until you switch it on.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            PermissionRow(
                title: "Microphone",
                detail: "Records only between the hotkey press and release (or its second press); the clip goes to your hub, nowhere else.",
                state: model.microphone,
                button: model.microphone == .notAsked ? "Allow…" : "Open Settings…",
                showButton: model.microphone != .granted,
                action: model.requestMicrophone)

            PermissionRow(
                title: "Accessibility",
                detail: "Lets DictateHub see the hotkey in other apps and paste the text at your cursor.",
                state: model.accessibility,
                button: "Open Accessibility Settings…",
                showButton: model.accessibility != .granted,
                action: model.openAccessibility)
            if model.accessibility != .granted && model.accessibilityRequested {
                Text("In the list, switch DictateHub on. This row turns green within a second.")
                    .font(.caption).foregroundStyle(.secondary).padding(.leading, 30)
            }

            hotkeyRow

            Divider()

            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    StatusMark(state: model.tokenPresent ? .granted : .notAsked)
                    Text("Hub token").font(.headline)
                    Spacer()
                }
                Text("The hub's API token, so only you can use your hub.")
                    .font(.caption).foregroundStyle(.secondary).padding(.leading, 30)
                HStack {
                    SecureField(model.tokenPresent ? "Saved; paste a new one to replace it" : "Paste the token",
                                text: $model.tokenInput)
                        .onSubmit { model.saveToken() }
                    Button("Save") { model.saveToken() }.disabled(model.tokenInput.isEmpty)
                }
                .padding(.leading, 30)
                Text(model.tokenNote ?? "or run `just mac-token` on the hub to install it over SSH")
                    .font(.caption).foregroundStyle(.secondary).padding(.leading, 30)
            }

            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    StatusMark(state: hubState)
                    Text("Hub connection").font(.headline)
                    Spacer()
                    Button("Test connection") { model.test() }
                }
                Text(model.hubLine ?? "Checks that the hub answers through the SSH tunnel.")
                    .font(.caption).foregroundStyle(.secondary).padding(.leading, 30)
                    .lineLimit(2)
            }

            HStack {
                Spacer()
                Button(model.checklist.isComplete ? "Done" : "Finish later") { model.done() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 500)
    }

    private var hubState: PermissionState {
        guard let line = model.hubLine else { return .notAsked }
        if line.hasPrefix("Hub OK") { return .granted }
        if line.hasPrefix("Test failed") { return .denied }
        return .notAsked
    }

    @ViewBuilder private var hotkeyRow: some View {
        let seen = model.lastGlobalPress.map { model.now.timeIntervalSince($0) < 600 } ?? false
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                StatusMark(state: seen ? .granted : .notAsked)
                Text("Hotkey test").font(.headline)
                Spacer()
            }
            Text(seen
                 ? "\(model.hotkeyName) works in other apps."
                 : "Click into another app (TextEdit, Notes), \(model.hotkeyMode == .hold ? "hold" : "press") \(model.hotkeyName), speak, \(model.hotkeyMode == .hold ? "release" : "press it again"). This row turns green when the key is seen.")
                .font(.caption).foregroundStyle(.secondary).padding(.leading, 30)
                .fixedSize(horizontal: false, vertical: true)
            if model.accessibility == .granted && !seen && !model.inputMonitoringGranted {
                HStack {
                    Text("Key not seen even with Accessibility on? Allow Input Monitoring too.")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Input Monitoring…") { model.openInputMonitoring() }.controlSize(.small)
                }
                .padding(.leading, 30)
            }
        }
    }
}

private struct PermissionRow: View {
    let title: String
    let detail: String
    let state: PermissionState
    let button: String
    let showButton: Bool
    let action: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                StatusMark(state: state)
                Text(title).font(.headline)
                Text(state.label).font(.caption).foregroundStyle(.secondary)
                Spacer()
                if showButton { Button(button, action: action) }
            }
            Text(detail).font(.caption).foregroundStyle(.secondary).padding(.leading, 30)
        }
    }
}

private struct StatusMark: View {
    let state: PermissionState

    var body: some View {
        Image(systemName: state.symbolName)
            .foregroundStyle(state == .granted ? .green : state == .denied ? .red : .secondary)
            .frame(width: 22)
    }
}
