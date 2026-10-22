import Foundation

/// Which key (or chord) is held to talk. Raw values are persisted in
/// `UserDefaults`, so never rename one.
public enum HotkeyChoice: String, CaseIterable, Sendable {
    case rightOption = "right_option"
    case fn = "fn"
    case rightCommand = "right_command"
    case controlOption = "control_option"

    public static let `default`: HotkeyChoice = .rightOption

    public var displayName: String {
        switch self {
        case .rightOption: return "Right Option (⌥)"
        case .fn: return "Fn / Globe"
        case .rightCommand: return "Right Command (⌘)"
        case .controlOption: return "Control + Option (⌃⌥)"
        }
    }

    /// Whether the hotkey is held, given an event's raw modifier flags
    /// (`NSEvent.modifierFlags.rawValue` / `CGEventFlags.rawValue`).
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
        }
    }
}

/// Modifier flag bits, spelled out so the core does not need AppKit.
public enum ModifierBits {
    public static let shift: UInt64 = 1 << 17
    public static let control: UInt64 = 1 << 18
    public static let option: UInt64 = 1 << 19
    public static let command: UInt64 = 1 << 20
    public static let function: UInt64 = 1 << 23
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
