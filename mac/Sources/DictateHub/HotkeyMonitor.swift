import AppKit
import CoreGraphics
import DictateHubCore

/// Global keyboard watch for the hotkey.
///
/// Modifier-only choices (hold to talk) use passive `NSEvent` monitors for
/// modifier changes and Esc; those need Accessibility trust (the same grant
/// paste needs) and cannot swallow events, so Esc also reaches the frontmost
/// app.
///
/// Chord choices (toggle to talk, e.g. Option+N) use an active
/// `CGEventTap` so the chord's keydown and keyup, and Esc while recording,
/// are consumed instead of reaching the focused app (no dead-key tilde, no
/// stray `n`). The tap needs Accessibility; when it cannot be created or is
/// switched off, `tapActive` is false and the controller says so in the
/// menu-bar status.
@MainActor
final class HotkeyMonitor {
    /// `true` when the press came from the global monitor (another app was
    /// frontmost), `false` from our own windows.
    var onPress: (_ global: Bool) -> Void = { _ in }
    var onRelease: () -> Void = {}
    var onEscape: () -> Void = {}
    /// Whether a dictation is recording; Esc is only swallowed then.
    var isRecording: () -> Bool = { false }

    private var detector: HotkeyEdgeDetector
    private var router: ChordKeyRouter
    private var monitors: [Any] = []
    private var tap: CFMachPort?
    private var tapSource: CFRunLoopSource?

    init(choice: HotkeyChoice) {
        detector = HotkeyEdgeDetector(choice: choice)
        router = ChordKeyRouter(chord: choice.chord)
    }

    var choice: HotkeyChoice {
        get { detector.choice }
        set {
            // Changing keys mid-hold must not leave a session stuck open.
            if detector.held { onRelease() }
            detector.choice = newValue
            detector.reset()
            router.chord = newValue.chord
            router.reset()
            if isStarted { start() }
        }
    }

    private var isStarted = false

    /// Whether the active choice listens through the event tap.
    var needsTap: Bool { detector.choice.chord != nil }

    /// Whether the event tap is installed and enabled. Meaningful only when
    /// `needsTap`.
    var tapActive: Bool {
        guard let tap else { return false }
        return CGEvent.tapIsEnabled(tap: tap)
    }

    /// Install the listeners for the current choice. Returns false when the
    /// choice needs the event tap and it could not be created.
    @discardableResult
    func start() -> Bool {
        stop()
        isStarted = true
        if needsTap { return installTap() }
        let mask: NSEvent.EventTypeMask = [.flagsChanged, .keyDown]
        if let global = NSEvent.addGlobalMonitorForEvents(matching: mask, handler: { [weak self] event in
            MainActor.assumeIsolated { self?.handle(event, global: true) }
        }) {
            monitors.append(global)
        }
        // Our own windows (Settings) get events locally, not globally.
        if let local = NSEvent.addLocalMonitorForEvents(matching: mask, handler: { [weak self] event in
            MainActor.assumeIsolated { self?.handle(event, global: false) }
            return event
        }) {
            monitors.append(local)
        }
        return true
    }

    func stop() {
        isStarted = false
        for m in monitors { NSEvent.removeMonitor(m) }
        monitors.removeAll()
        removeTap()
        detector.reset()
        router.reset()
    }

    private func handle(_ event: NSEvent, global: Bool) {
        switch event.type {
        case .flagsChanged:
            switch detector.update(flags: UInt64(event.modifierFlags.rawValue)) {
            case .pressed: onPress(global)
            case .released: onRelease()
            case nil: break
            }
        case .keyDown where event.keyCode == KeyCode.escape:
            onEscape()
        default:
            break
        }
    }

    // MARK: event tap (chord choices)

    private func installTap() -> Bool {
        let mask: CGEventMask = (1 << CGEventType.keyDown.rawValue) | (1 << CGEventType.keyUp.rawValue)
        let refcon = Unmanaged.passUnretained(self).toOpaque()
        guard let port = CGEvent.tapCreate(
            tap: .cgSessionEventTap, place: .headInsertEventTap, options: .defaultTap,
            eventsOfInterest: mask, callback: hotkeyTapCallback, userInfo: refcon)
        else {
            Log.error("event tap: could not be created (Accessibility or Input Monitoring missing?)")
            return false
        }
        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, port, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: port, enable: true)
        tap = port
        tapSource = source
        Log.info("event tap installed for \(detector.choice.rawValue)")
        return true
    }

    private func removeTap() {
        if let tap { CGEvent.tapEnable(tap: tap, enable: false) }
        if let tapSource { CFRunLoopRemoveSource(CFRunLoopGetMain(), tapSource, .commonModes) }
        tap = nil
        tapSource = nil
    }

    /// Called on the main thread from the tap callback. Returns true to
    /// swallow the event. The press and Esc actions run after the callback
    /// returns so slow work (microphone start) cannot make the system
    /// disable the tap.
    fileprivate func handleTap(type: CGEventType, keyCode: UInt16, flags: UInt64, isRepeat: Bool) -> Bool {
        switch type {
        case .tapDisabledByTimeout, .tapDisabledByUserInput:
            Log.error("event tap disabled by the system (\(type == .tapDisabledByTimeout ? "timeout" : "user input")); re-enabling")
            router.reset()
            if let tap { CGEvent.tapEnable(tap: tap, enable: true) }
            return false
        case .keyDown, .keyUp:
            let route = router.route(type == .keyDown ? .keyDown : .keyUp, keyCode: keyCode, flags: flags,
                                     isRepeat: isRepeat, recording: isRecording())
            guard case .swallow(let trigger) = route else { return false }
            let ownPID = ProcessInfo.processInfo.processIdentifier
            let global = NSWorkspace.shared.frontmostApplication?.processIdentifier != ownPID
            switch trigger {
            case .press:
                DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.onPress(global) } }
            case .escape:
                DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.onEscape() } }
            case nil:
                break
            }
            return true
        default:
            return false
        }
    }
}

/// C callback for the event tap; it runs on the main run loop (the tap's
/// source lives there), so it may touch main-actor state.
private let hotkeyTapCallback: CGEventTapCallBack = { _, type, event, refcon in
    guard let refcon else { return Unmanaged.passUnretained(event) }
    let keyCode = UInt16(truncatingIfNeeded: event.getIntegerValueField(.keyboardEventKeycode))
    let flags = event.flags.rawValue
    let isRepeat = event.getIntegerValueField(.keyboardEventAutorepeat) != 0
    let address = UInt(bitPattern: refcon)
    let swallow = MainActor.assumeIsolated {
        Unmanaged<HotkeyMonitor>.fromOpaque(UnsafeRawPointer(bitPattern: address)!).takeUnretainedValue()
            .handleTap(type: type, keyCode: keyCode, flags: flags, isRepeat: isRepeat)
    }
    return swallow ? nil : Unmanaged.passUnretained(event)
}
