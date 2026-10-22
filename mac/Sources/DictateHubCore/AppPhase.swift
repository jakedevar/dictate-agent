import Foundation

/// What the menu-bar icon shows.
public enum AppPhase: Equatable, Sendable {
    case idle
    case recording
    case transcribing
    case error(String)

    /// SF Symbol for the status item.
    public var symbolName: String {
        switch self {
        case .idle: return "mic"
        case .recording: return "mic.fill"
        case .transcribing: return "ellipsis.circle"
        case .error: return "exclamationmark.triangle"
        }
    }

    /// Frames the status item cycles through while busy (one frame = static).
    public var animationFrames: [String] {
        switch self {
        case .recording: return ["mic.fill", "mic"]
        case .transcribing: return ["ellipsis.circle", "ellipsis.circle.fill"]
        case .idle, .error: return [symbolName]
        }
    }

    /// Seconds per animation frame.
    public static let frameInterval: Double = 0.45

    /// The menu's first (disabled) line.
    public var statusLine: String { statusLine(mode: .hold) }

    /// The status line, with the recording hint for the active hotkey mode.
    public func statusLine(mode: HotkeyMode) -> String {
        switch self {
        case .idle: return "Ready"
        case .recording:
            return mode == .hold
                ? "Recording… (release to send, Esc to cancel)"
                : "Recording… (press the hotkey to send, Esc to cancel)"
        case .transcribing: return "Transcribing on the hub…"
        case .error(let message): return message
        }
    }
}
