import AppKit
import ApplicationServices
import DictateHubCore

/// Paste at the cursor: snapshot the general pasteboard, put the text on it,
/// synthesize Cmd+V, and restore the snapshot ~300 ms later.
@MainActor
enum TextInserter {
    static let restoreDelay: Duration = .milliseconds(300)

    /// Whether this process may post keyboard events (Accessibility).
    static func isTrusted(prompt: Bool = false) -> Bool {
        let key = "AXTrustedCheckOptionPrompt" as CFString
        return AXIsProcessTrustedWithOptions([key: prompt] as CFDictionary)
    }

    static func insert(_ text: String) async {
        let swap = PasteboardSwap.begin(text: text, on: .general)
        postCommandV()
        try? await Task.sleep(for: restoreDelay)
        if !swap.finish() {
            Log.info("pasteboard changed during paste; left as is")
        }
    }

    private static func postCommandV() {
        let source = CGEventSource(stateID: .hidSystemState)
        let key = CGKeyCode(KeyCode.ansiV)
        guard let down = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: true),
              let up = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: false)
        else { return }
        // Explicit flags: a still-held hotkey modifier (e.g. Control in
        // Control+Option) must not turn this into a different shortcut.
        down.flags = .maskCommand
        up.flags = .maskCommand
        down.post(tap: .cghidEventTap)
        up.post(tap: .cghidEventTap)
    }
}
