import Foundation

/// How a hotkey drives a dictation.
public enum HotkeyMode: Equatable, Sendable {
    /// Record while the key is held; release sends.
    case hold
    /// First press starts, the next press (or Esc) stops.
    case toggle

    /// Short label shown next to each choice in the choices UI.
    public var label: String {
        switch self {
        case .hold: return "hold to talk"
        case .toggle: return "press to start, press again to stop"
        }
    }
}

/// Which key (or chord) starts a dictation. Raw values are persisted in
/// `UserDefaults`, so never rename one.
public enum HotkeyChoice: String, CaseIterable, Sendable {
    case rightOption = "right_option"
    case fn = "fn"
    case rightCommand = "right_command"
    case controlOption = "control_option"
    /// A key+modifier chord (N with only Option held); toggles.
    case optionN = "option_n"

    public static let `default`: HotkeyChoice = .rightOption

    public var displayName: String {
        switch self {
        case .rightOption: return "Right Option (⌥)"
        case .fn: return "Fn / Globe"
        case .rightCommand: return "Right Command (⌘)"
        case .controlOption: return "Control + Option (⌃⌥)"
        case .optionN: return "Option + N (⌥N)"
        }
    }

    public var mode: HotkeyMode {
        switch self {
        case .optionN: return .toggle
        case .rightOption, .fn, .rightCommand, .controlOption: return .hold
        }
    }

    /// The choices UI line: name and mode, e.g. "Option + N (⌥N), press to start, press again to stop".
    public var pickerLabel: String { "\(displayName): \(mode.label)" }

    /// The menu hint under the status line.
    public var hint: String {
        switch mode {
        case .hold: return "Hold \(displayName) to dictate"
        case .toggle: return "Press \(displayName) to start, again (or Esc) to stop"
        }
    }

    /// The chord this choice listens for, nil for modifier-only choices.
    public var chord: HotkeyChord? {
        switch self {
        case .optionN: return HotkeyChord(keyCode: KeyCode.ansiN, modifiers: ModifierBits.option)
        case .rightOption, .fn, .rightCommand, .controlOption: return nil
        }
    }

    /// Whether the hotkey is held, given an event's raw modifier flags
    /// (`NSEvent.modifierFlags.rawValue` / `CGEventFlags.rawValue`).
    /// Chord choices are never "held": they are matched on key events.
    ///
    /// Right-hand keys are told apart by the device-dependent bits in the low
    /// word (IOKit `NX_DEVICER*KEYMASK`), which AppKit and Quartz both pass
    /// through; the device-independent bits cannot tell left from right.
    public func isHeld(flags: UInt64) -> Bool {
        switch self {
        case .rightOption: return flags & ModifierBits.deviceRightOption != 0
        case .rightCommand: return flags & ModifierBits.deviceRightCommand != 0
        case .fn: return flags & ModifierBits.function != 0
        case .controlOption:
            return flags & ModifierBits.control != 0 && flags & ModifierBits.option != 0
        case .optionN: return false
        }
    }
}

/// A key plus exactly the modifiers in `modifiers` (shift, control, option,
/// command only: Caps Lock, Fn and device bits are ignored).
public struct HotkeyChord: Equatable, Sendable {
    public let keyCode: UInt16
    public let modifiers: UInt64

    public init(keyCode: UInt16, modifiers: UInt64) {
        self.keyCode = keyCode
        self.modifiers = modifiers
    }

    public func matches(keyCode: UInt16, flags: UInt64) -> Bool {
        keyCode == self.keyCode && flags & ModifierBits.chordMask == modifiers
    }
}

/// Modifier flag bits, spelled out so the core does not need AppKit.
public enum ModifierBits {
    public static let shift: UInt64 = 1 << 17
    public static let control: UInt64 = 1 << 18
    public static let option: UInt64 = 1 << 19
    public static let command: UInt64 = 1 << 20
    public static let function: UInt64 = 1 << 23
    /// The four modifiers a chord is matched against (not Caps Lock, Fn or device bits).
    public static let chordMask: UInt64 = shift | control | option | command
    // IOKit/hidsystem/IOLLEvent.h device-dependent masks.
    public static let deviceLeftControl: UInt64 = 0x0000_0001
    public static let deviceLeftShift: UInt64 = 0x0000_0002
    public static let deviceRightShift: UInt64 = 0x0000_0004
    public static let deviceLeftCommand: UInt64 = 0x0000_0008
    public static let deviceRightCommand: UInt64 = 0x0000_0010
    public static let deviceLeftOption: UInt64 = 0x0000_0020
    public static let deviceRightOption: UInt64 = 0x0000_0040
    public static let deviceRightControl: UInt64 = 0x0000_2000
}

/// Virtual key codes the app needs (Carbon `kVK_*`).
public enum KeyCode {
    public static let escape: UInt16 = 53
    public static let ansiV: UInt16 = 9
    public static let ansiN: UInt16 = 45
}

/// Turns a stream of raw modifier flags into press/release edges for one
/// `HotkeyChoice`. `flagsChanged` fires for every modifier, so the edge is
/// computed from the held state, not from the key code.
public struct HotkeyEdgeDetector: Sendable {
    public enum Edge: Equatable, Sendable { case pressed, released }

    public var choice: HotkeyChoice
    public private(set) var held = false

    public init(choice: HotkeyChoice) { self.choice = choice }

    public mutating func update(flags: UInt64) -> Edge? {
        let now = choice.isHeld(flags: flags)
        defer { held = now }
        switch (held, now) {
        case (false, true): return .pressed
        case (true, false): return .released
        default: return nil
        }
    }

    /// Forget the held state (the hotkey choice changed, or the app lost
    /// track of the keyboard).
    public mutating func reset() { held = false }
}

/// Hold-to-talk session logic, independent of AppKit and audio:
/// press starts capture, release sends it, a release under `minimumHold`
/// discards it (an accidental tap), and Esc while held cancels it.
public struct HoldToTalk: Sendable {
    public enum Action: Equatable, Sendable {
        /// Begin capturing audio.
        case start
        /// Stop capturing and send the clip.
        case finish
        /// Stop capturing and drop the clip: the key was only tapped.
        case discard
        /// Stop capturing and drop the clip: the user pressed Esc.
        case cancel
    }

    public static let defaultMinimumHold: TimeInterval = 0.25

    public let minimumHold: TimeInterval
    public private(set) var pressedAt: TimeInterval?
    /// True after Esc until the hotkey is released, so the release does not
    /// also send.
    public private(set) var cancelled = false

    public init(minimumHold: TimeInterval = HoldToTalk.defaultMinimumHold) {
        self.minimumHold = minimumHold
    }

    public var isCapturing: Bool { pressedAt != nil && !cancelled }

    public mutating func press(at time: TimeInterval) -> Action? {
        guard pressedAt == nil else { return nil }
        pressedAt = time
        cancelled = false
        return .start
    }

    public mutating func release(at time: TimeInterval) -> Action? {
        guard let start = pressedAt else { return nil }
        pressedAt = nil
        if cancelled {
            cancelled = false
            return nil
        }
        return time - start < minimumHold ? .discard : .finish
    }

    public mutating func escape() -> Action? {
        guard isCapturing else { return nil }
        cancelled = true
        return .cancel
    }

    /// Capture ended on its own (the length cap): treat it as a release that
    /// sends, and swallow the real release when it comes.
    public mutating func limitReached() -> Action? {
        guard isCapturing else { return nil }
        cancelled = true
        return .finish
    }
}

/// Toggle-to-talk session logic for chord hotkeys: the first press starts
/// capture, the next press sends it, and Esc while capturing cancels it.
public struct ToggleToTalk: Sendable {
    public private(set) var isCapturing = false

    public init() {}

    public mutating func press() -> HoldToTalk.Action? {
        if isCapturing {
            isCapturing = false
            return .finish
        }
        isCapturing = true
        return .start
    }

    public mutating func escape() -> HoldToTalk.Action? {
        guard isCapturing else { return nil }
        isCapturing = false
        return .cancel
    }

    /// Capture ended on its own (the length cap): it sends.
    public mutating func limitReached() -> HoldToTalk.Action? {
        guard isCapturing else { return nil }
        isCapturing = false
        return .finish
    }

    /// Capture could not start or was dropped: forget it without an action.
    public mutating func reset() { isCapturing = false }
}

/// Decides, per key event seen by the event tap, whether a chord hotkey
/// consumes it. Pure, so the swallowing rules are tested without a tap.
///
/// The chord's keydown and keyup are both swallowed (no dead-key tilde or
/// stray `n` reaches the focused app); the keyup is swallowed even when the
/// modifier was released first. Esc is swallowed only while recording, and
/// its keyup follows its keydown.
public struct ChordKeyRouter: Sendable {
    public enum Event: Equatable, Sendable { case keyDown, keyUp }
    public enum Trigger: Equatable, Sendable { case press, escape }
    public enum Route: Equatable, Sendable {
        /// Deliver the event to the focused app.
        case passThrough
        /// Consume it (return nil from the tap); `trigger` is the action it carries.
        case swallow(Trigger?)
    }

    public var chord: HotkeyChord?
    private var chordDown = false
    private var escapeDown = false

    public init(chord: HotkeyChord?) { self.chord = chord }

    public mutating func route(_ event: Event, keyCode: UInt16, flags: UInt64,
                               isRepeat: Bool, recording: Bool) -> Route {
        guard let chord else { return .passThrough }
        switch event {
        case .keyDown:
            if chord.matches(keyCode: keyCode, flags: flags) {
                // Auto-repeat of a held chord is consumed but not a new press.
                let fresh = !isRepeat && !chordDown
                chordDown = true
                return .swallow(fresh ? .press : nil)
            }
            if keyCode == KeyCode.escape && recording {
                let fresh = !isRepeat && !escapeDown
                escapeDown = true
                return .swallow(fresh ? .escape : nil)
            }
            return .passThrough
        case .keyUp:
            if keyCode == chord.keyCode && chordDown {
                chordDown = false
                return .swallow(nil)
            }
            if keyCode == KeyCode.escape && escapeDown {
                escapeDown = false
                return .swallow(nil)
            }
            return .passThrough
        }
    }

    /// Forget which keys are down (the tap was reinstalled or the choice changed).
    public mutating func reset() {
        chordDown = false
        escapeDown = false
    }
}
