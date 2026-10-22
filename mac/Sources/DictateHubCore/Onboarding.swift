import Foundation

/// One permission row's state in the onboarding window.
public enum PermissionState: Equatable, Sendable {
    case granted
    case notAsked
    case denied

    public var label: String {
        switch self {
        case .granted: return "Granted"
        case .notAsked: return "Not granted yet"
        case .denied: return "Denied"
        }
    }

    /// SF Symbol for the row's status mark.
    public var symbolName: String {
        switch self {
        case .granted: return "checkmark.circle.fill"
        case .notAsked: return "circle.dashed"
        case .denied: return "xmark.circle.fill"
        }
    }
}

/// What the onboarding window shows, and whether the app should open it at
/// launch: on the first run, or whenever something dictation needs is missing.
public struct OnboardingChecklist: Equatable, Sendable {
    public var microphone: PermissionState
    public var accessibility: PermissionState
    public var tokenPresent: Bool

    public init(microphone: PermissionState, accessibility: PermissionState, tokenPresent: Bool) {
        self.microphone = microphone
        self.accessibility = accessibility
        self.tokenPresent = tokenPresent
    }

    /// Everything a dictation needs is in place.
    public var isComplete: Bool {
        microphone == .granted && accessibility == .granted && tokenPresent
    }

    public func shouldOpenAtLaunch(firstRun: Bool) -> Bool {
        firstRun || !isComplete
    }
}

/// System Settings deep links (Privacy & Security panes).
public enum PrivacyPane: String, Sendable, CaseIterable {
    case microphone = "Privacy_Microphone"
    case accessibility = "Privacy_Accessibility"
    case inputMonitoring = "Privacy_ListenEvent"

    public var url: URL {
        URL(string: "x-apple.systempreferences:com.apple.preference.security?\(rawValue)")!
    }
}
