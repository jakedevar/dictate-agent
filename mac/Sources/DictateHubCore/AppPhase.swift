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

    /// The menu's first (disabled) line.
    public var statusLine: String {
        switch self {
        case .idle: return "Ready"
        case .recording: return "Recording… (release to send, Esc to cancel)"
        case .transcribing: return "Transcribing on the hub…"
        case .error(let message): return message
        }
    }
}
