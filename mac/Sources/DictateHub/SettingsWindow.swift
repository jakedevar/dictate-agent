import AppKit
import DictateHubCore
import SwiftUI

/// Settings: hub endpoint, hotkey, API token, connection test, feedback
/// sounds, warm microphone, open at login. Permissions live in their own
/// window (menu → Permissions…).
@MainActor
final class SettingsWindowController {
    private let model: SettingsModel
    private var window: NSWindow?

    init(model: SettingsModel) {
        self.model = model
    }

    func show() {
        model.reload()
        if window == nil {
            let w = NSWindow(contentViewController: NSHostingController(rootView: SettingsView(model: model)))
            w.title = "DictateHub Settings"
            w.styleMask = [.titled, .closable]
            w.isReleasedWhenClosed = false
            w.center()
            window = w
        }
        NSApp.activate()
        window?.makeKeyAndOrderFront(nil)
    }

    func showResult(_ line: String) {
        model.testResult = line
    }
}

@MainActor
@Observable
final class SettingsModel {
    let settings: HubSettings
    let tokens: TokenStore
    var onHotkeyChange: (HotkeyChoice) -> Void = { _ in }
    var onTest: () -> Void = {}
    var onKeepWarmChange: (Bool) -> Void = { _ in }
    var onOpenPermissions: () -> Void = {}

    var endpoint = ""
    var hotkey: HotkeyChoice = .default
    var tokenInput = ""
    var tokenPresent = false
    var tokenNote: String?
    var testResult: String?
    var playSounds = true
    var keepMicWarm = false
    var launchAtLogin = false
    var loginNote: String?

    init(settings: HubSettings, tokens: TokenStore) {
        self.settings = settings
        self.tokens = tokens
        reload()
    }

    func reload() {
        endpoint = settings.endpoint
        hotkey = settings.hotkey
        tokenPresent = tokens.load() != nil
        tokenInput = ""
        playSounds = settings.playSounds
        keepMicWarm = settings.keepMicWarm
        launchAtLogin = LoginItem.isEnabled
        loginNote = loginStatusNote()
    }

    func saveEndpoint() {
        let trimmed = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        settings.endpoint = trimmed.isEmpty ? HubRequestBuilder.defaultEndpoint : trimmed
        endpoint = settings.endpoint
    }

    func saveHotkey(_ choice: HotkeyChoice) {
        settings.hotkey = choice
        onHotkeyChange(choice)
    }

    func saveToken() {
        guard !tokenInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        do {
            try tokens.save(tokenInput)
            tokenInput = ""
            tokenPresent = true
            tokenNote = "Saved to \(tokens.fileURL.path) (0600)"
        } catch {
            tokenNote = "Could not save the token: \(error.localizedDescription)"
        }
    }

    func test() {
        saveEndpoint()
        testResult = "Testing…"
        onTest()
    }

    func setPlaySounds(_ on: Bool) {
        playSounds = on
        settings.playSounds = on
    }

    func setKeepMicWarm(_ on: Bool) {
        keepMicWarm = on
        settings.keepMicWarm = on
        onKeepWarmChange(on)
    }

    func setLaunchAtLogin(_ on: Bool) {
        do {
            try LoginItem.set(on)
            settings.loginItemConfigured = true
        } catch {
            Log.error("login item \(on ? "register" : "unregister"): \(error.localizedDescription)")
        }
        launchAtLogin = LoginItem.isEnabled
        loginNote = loginStatusNote()
    }

    private func loginStatusNote() -> String? {
        if !LoginItem.isInstalledLocation {
            return "Only the installed copy (~/Applications/DictateHub.app) opens at login."
        }
        if LoginItem.status == .requiresApproval {
            return "Waiting for approval in System Settings → General → Login Items."
        }
        return nil
    }
}

struct SettingsView: View {
    @Bindable var model: SettingsModel

    var body: some View {
        Form {
            Section("Hub") {
                Picker("Connection", selection: Binding(
                    get: { model.endpoint == HubRequestBuilder.defaultEndpoint },
                    set: { model.endpoint = $0 ? HubRequestBuilder.defaultEndpoint : "https://"; model.saveEndpoint() })) {
                    Text("Hub (secure tunnel)").tag(true)
                    Text("Custom HTTP(S) URL").tag(false)
                }
                if model.endpoint != HubRequestBuilder.defaultEndpoint {
                    TextField("Endpoint", text: $model.endpoint)
                        .onSubmit { model.saveEndpoint() }
                }
                Text("The secure tunnel connects to the hub through SSH.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("Hotkey") {
                Picker("Hotkey", selection: Binding(
                    get: { model.hotkey },
                    set: { model.hotkey = $0; model.saveHotkey($0) })) {
                    ForEach(HotkeyChoice.allCases, id: \.self) { Text($0.pickerLabel).tag($0) }
                }
            }
            Section("API token") {
                SecureField(model.tokenPresent ? "Token saved; paste a new one to replace it" : "Paste the hub's API token",
                            text: $model.tokenInput)
                    .onSubmit { model.saveToken() }
                HStack {
                    Button("Save token") { model.saveToken() }
                        .disabled(model.tokenInput.isEmpty)
                    if let note = model.tokenNote {
                        Text(note).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Text("Or run `just mac-token` on the hub (it runs dictated --api-token and installs it over SSH).")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section {
                HStack {
                    Button("Test connection") { model.test() }
                    if let result = model.testResult {
                        Text(result).font(.caption).lineLimit(2)
                    }
                }
            }
            Section("Feedback") {
                Toggle("Start and stop sounds", isOn: Binding(
                    get: { model.playSounds }, set: { model.setPlaySounds($0) }))
            }
            Section("Microphone") {
                Toggle("Keep the microphone ready between dictations", isOn: Binding(
                    get: { model.keepMicWarm }, set: { model.setKeepMicWarm($0) }))
                Text("Keeps the last 0.3 s before the hotkey, so a first word spoken at key-down is not cut. "
                     + "The microphone then stays open while DictateHub runs: the orange privacy dot stays on, "
                     + "and Bluetooth headphones (AirPods) stay in their lower-quality call mode. "
                     + "Off: the microphone opens on key-down and the start sound plays when it is live.")
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Section("Startup") {
                Toggle("Open DictateHub at login", isOn: Binding(
                    get: { model.launchAtLogin }, set: { model.setLaunchAtLogin($0) }))
                    .disabled(!LoginItem.isInstalledLocation)
                if let note = model.loginNote {
                    HStack {
                        Text(note).font(.caption).foregroundStyle(.secondary)
                        if LoginItem.status == .requiresApproval {
                            Button("Open Login Items…") { LoginItem.openSystemSettings() }.controlSize(.small)
                        }
                    }
                }
            }
            Section {
                Button("Permissions…") { model.onOpenPermissions() }
            }
        }
        .formStyle(.grouped)
        .frame(width: 480)
        .padding(.vertical, 8)
    }
}
