import AppKit
import DictateHubCore
import SwiftUI

/// Minimal M1 settings: hub endpoint, hotkey, API token, connection test.
/// (M2 replaces this with the onboarding flow.)
@MainActor
final class SettingsWindowController {
    private let model: SettingsModel
    private var window: NSWindow?

    init(settings: HubSettings, tokens: TokenStore,
         onHotkeyChange: @escaping (HotkeyChoice) -> Void,
         onTest: @escaping () -> Void) {
        model = SettingsModel(settings: settings, tokens: tokens,
                              onHotkeyChange: onHotkeyChange, onTest: onTest)
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
    let onHotkeyChange: (HotkeyChoice) -> Void
    let onTest: () -> Void

    var endpoint = ""
    var hotkey: HotkeyChoice = .default
    var tokenInput = ""
    var tokenPresent = false
    var tokenNote: String?
    var testResult: String?

    init(settings: HubSettings, tokens: TokenStore,
         onHotkeyChange: @escaping (HotkeyChoice) -> Void, onTest: @escaping () -> Void) {
        self.settings = settings
        self.tokens = tokens
        self.onHotkeyChange = onHotkeyChange
        self.onTest = onTest
        reload()
    }

    func reload() {
        endpoint = settings.endpoint
        hotkey = settings.hotkey
        tokenPresent = tokens.load() != nil
        tokenInput = ""
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
}

struct SettingsView: View {
    @Bindable var model: SettingsModel

    var body: some View {
        Form {
            Section("Hub") {
                TextField("Endpoint", text: $model.endpoint, prompt: Text(HubRequestBuilder.defaultEndpoint))
                    .onSubmit { model.saveEndpoint() }
                Text("Default \(HubRequestBuilder.defaultEndpoint) is the hub's API through the SSH tunnel.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("Hotkey") {
                Picker("Hold to talk", selection: Binding(
                    get: { model.hotkey },
                    set: { model.hotkey = $0; model.saveHotkey($0) })) {
                    ForEach(HotkeyChoice.allCases, id: \.self) { Text($0.displayName).tag($0) }
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
                Text("On the hub: dictated --api-token").font(.caption).foregroundStyle(.secondary)
            }
            Section {
                HStack {
                    Button("Test connection") { model.test() }
                    if let result = model.testResult {
                        Text(result).font(.caption).lineLimit(2)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .padding(.vertical, 8)
    }
}
