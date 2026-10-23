import Foundation

/// The three system permissions dictation can be missing.
public enum MissingPermission: String, Equatable, Sendable, CaseIterable {
    case accessibility
    case inputMonitoring
    case microphone

    public var title: String {
        switch self {
        case .accessibility: return "Accessibility"
        case .inputMonitoring: return "Input Monitoring"
        case .microphone: return "Microphone"
        }
    }

    /// Where the user turns it on.
    public var settingsPath: String {
        "System Settings → Privacy & Security → \(title)"
    }

    /// A grant made for an older build is pinned to that build's code
    /// signature and does not carry over, although the list still shows
    /// DictateHub switched on. Only the two key-event permissions work that way
    /// for this app; the microphone grant follows the designated requirement.
    public var staleGrantHint: String? {
        switch self {
        case .accessibility, .inputMonitoring:
            return "If DictateHub already looks enabled there, remove it with − and add "
                + "/Applications/DictateHub.app again with + (a grant made for an older build does not carry over)."
        case .microphone:
            return nil
        }
    }
}

/// What the app can observe about its own trust right now.
public struct PermissionSnapshot: Equatable, Sendable {
    /// `AXIsProcessTrusted`.
    public var accessibility: Bool
    /// `CGPreflightListenEventAccess`.
    public var inputMonitoring: Bool
    /// `AVCaptureDevice.authorizationStatus(for: .audio)`, mapped.
    public var microphone: PermissionState
    /// The chosen hotkey listens through an event tap, which needs Input
    /// Monitoring as well as Accessibility. Hold-to-talk keys use passive
    /// monitors and need Accessibility only.
    public var hotkeyNeedsInputMonitoring: Bool

    public init(accessibility: Bool, inputMonitoring: Bool, microphone: PermissionState,
                hotkeyNeedsInputMonitoring: Bool) {
        self.accessibility = accessibility
        self.inputMonitoring = inputMonitoring
        self.microphone = microphone
        self.hotkeyNeedsInputMonitoring = hotkeyNeedsInputMonitoring
    }
}

/// Which permissions are missing and the message that names them. Pure, so the
/// wording is tested; the app only feeds it a snapshot.
public struct PermissionDiagnosis: Equatable, Sendable {
    public var missing: [MissingPermission]

    public init(_ snapshot: PermissionSnapshot) {
        var missing: [MissingPermission] = []
        if !snapshot.accessibility { missing.append(.accessibility) }
        if snapshot.hotkeyNeedsInputMonitoring && !snapshot.inputMonitoring {
            missing.append(.inputMonitoring)
        }
        if snapshot.microphone != .granted { missing.append(.microphone) }
        self.missing = missing
    }

    public var isComplete: Bool { missing.isEmpty }

    /// One line per missing permission: name, effect, path, and for the two
    /// key-event permissions the stale-grant hint.
    public func lines() -> [String] {
        missing.map(Self.line)
    }

    /// The full text for the Permissions window; nil when nothing is missing.
    public var detail: String? {
        isComplete ? nil : lines().joined(separator: "\n")
    }

    /// The menu-bar error headline: every missing permission and what stops
    /// working. nil when nothing is missing. The per-permission paths and the
    /// stale-grant hint follow as `lines()`.
    public var headline: String? {
        guard !missing.isEmpty else { return nil }
        let names = missing.map(\.title).joined(separator: " + ")
        return "\(names) not granted: \(Self.effect(of: missing))"
    }

    private static func line(_ permission: MissingPermission) -> String {
        var text = "\(permission.title) is not granted. Turn it on in \(permission.settingsPath)."
        if let hint = permission.staleGrantHint { text += " " + hint }
        return text
    }

    private static func effect(of missing: [MissingPermission]) -> String {
        var effects: [String] = []
        if missing.contains(.accessibility) { effects.append("hotkey and paste are off") }
        if missing.contains(.inputMonitoring) && !missing.contains(.accessibility) {
            effects.append("the hotkey cannot see its key")
        }
        if missing.contains(.microphone) { effects.append("it cannot record") }
        return effects.joined(separator: ", ")
    }
}

/// A one-line warning when AeroSpace (the tiling window manager) binds
/// Option+N, which would take the key before DictateHub sees it. Reads text
/// only; the app never edits that file.
public enum AeroSpaceConflict {
    public static let warning =
        "AeroSpace binds alt-n in ~/.config/aerospace/aerospace.toml and may take Option+N before DictateHub sees it."

    /// True when an uncommented line binds `alt-n` (alone, not alt-shift-n).
    public static func bindsAltN(in config: String) -> Bool {
        for raw in config.split(whereSeparator: \.isNewline) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("#") { continue }
            // `alt-n = ...` or `'alt-n' = ...` / `"alt-n" = ...`
            guard let eq = line.firstIndex(of: "=") else { continue }
            let key = line[..<eq].trimmingCharacters(in: CharacterSet.whitespaces.union(CharacterSet(charactersIn: "'\"")))
            if key == "alt-n" { return true }
        }
        return false
    }
}

/// Greedy word wrap for menu items, which do not wrap long titles.
public func wrapForMenu(_ text: String, width: Int = 64) -> [String] {
    var lines: [String] = []
    var current = ""
    for word in text.split(separator: " ") {
        if current.isEmpty {
            current = String(word)
        } else if current.count + 1 + word.count <= width {
            current += " " + word
        } else {
            lines.append(current)
            current = String(word)
        }
    }
    if !current.isEmpty { lines.append(current) }
    return lines
}
