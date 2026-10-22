import AppKit
import DictateHubCore

/// Passive global keyboard watch: modifier changes for the hold-to-talk
/// hotkey, and Esc. `NSEvent` global monitors need Accessibility trust (the
/// same grant paste needs) and cannot swallow events, so Esc also reaches the
/// frontmost app.
@MainActor
final class HotkeyMonitor {
    var onPress: () -> Void = {}
    var onRelease: () -> Void = {}
    var onEscape: () -> Void = {}

    private var detector: HotkeyEdgeDetector
    private var monitors: [Any] = []

    init(choice: HotkeyChoice) {
        detector = HotkeyEdgeDetector(choice: choice)
    }

    var choice: HotkeyChoice {
        get { detector.choice }
        set {
            // Changing keys mid-hold must not leave a session stuck open.
            if detector.held { onRelease() }
            detector.choice = newValue
            detector.reset()
        }
    }

    func start() {
        stop()
        let mask: NSEvent.EventTypeMask = [.flagsChanged, .keyDown]
        if let global = NSEvent.addGlobalMonitorForEvents(matching: mask, handler: { [weak self] event in
            MainActor.assumeIsolated { self?.handle(event) }
        }) {
            monitors.append(global)
        }
        // Our own windows (Settings) get events locally, not globally.
        if let local = NSEvent.addLocalMonitorForEvents(matching: mask, handler: { [weak self] event in
            MainActor.assumeIsolated { self?.handle(event) }
            return event
        }) {
            monitors.append(local)
        }
    }

    func stop() {
        for m in monitors { NSEvent.removeMonitor(m) }
        monitors.removeAll()
        detector.reset()
    }

    private func handle(_ event: NSEvent) {
        switch event.type {
        case .flagsChanged:
            switch detector.update(flags: UInt64(event.modifierFlags.rawValue)) {
            case .pressed: onPress()
            case .released: onRelease()
            case nil: break
            }
        case .keyDown where event.keyCode == KeyCode.escape:
            onEscape()
        default:
            break
        }
    }
}
