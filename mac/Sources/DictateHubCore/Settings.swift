import Foundation

/// User settings, kept in `UserDefaults`. The token is *not* here: it lives
/// in a 0600 file (see `TokenStore`).
public struct HubSettings: @unchecked Sendable {
    // UserDefaults is documented thread-safe; the wrapper only reads/writes it.
    public static let endpointKey = "hubEndpoint"
    public static let hotkeyKey = "hotkey"
    public static let playSoundsKey = "playSounds"
    public static let keepMicWarmKey = "keepMicWarm"
    public static let onboardingDoneKey = "onboardingDone"
    public static let loginItemConfiguredKey = "loginItemConfigured"

    let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) { self.defaults = defaults }

    public var endpoint: String {
        get {
            let s = defaults.string(forKey: Self.endpointKey)?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            return s.isEmpty ? HubRequestBuilder.defaultEndpoint : s
        }
        nonmutating set { defaults.set(newValue, forKey: Self.endpointKey) }
    }

    public var hotkey: HotkeyChoice {
        get {
            defaults.string(forKey: Self.hotkeyKey).flatMap(HotkeyChoice.init(rawValue:))
                ?? .default
        }
        nonmutating set { defaults.set(newValue.rawValue, forKey: Self.hotkeyKey) }
    }

    /// Start/stop/error sounds. Default on.
    public var playSounds: Bool {
        get { bool(Self.playSoundsKey, default: true) }
        nonmutating set { defaults.set(newValue, forKey: Self.playSoundsKey) }
    }

    /// Keep the microphone open while idle, with a short pre-roll ring, so a
    /// word spoken right at key-down is kept. Default off: an open microphone
    /// shows the orange privacy indicator the whole time, and a Bluetooth
    /// headset stays in its low-quality call profile.
    public var keepMicWarm: Bool {
        get { bool(Self.keepMicWarmKey, default: false) }
        nonmutating set { defaults.set(newValue, forKey: Self.keepMicWarmKey) }
    }

    /// The permissions window has been shown once (first run).
    public var onboardingDone: Bool {
        get { bool(Self.onboardingDoneKey, default: false) }
        nonmutating set { defaults.set(newValue, forKey: Self.onboardingDoneKey) }
    }

    /// The login item has been registered once (default on). After that the
    /// system's own state is the truth: turning it off in System Settings is
    /// respected and never undone at the next launch.
    public var loginItemConfigured: Bool {
        get { bool(Self.loginItemConfiguredKey, default: false) }
        nonmutating set { defaults.set(newValue, forKey: Self.loginItemConfiguredKey) }
    }

    private func bool(_ key: String, default value: Bool) -> Bool {
        defaults.object(forKey: key) == nil ? value : defaults.bool(forKey: key)
    }
}

/// The bearer token, in `~/Library/Application Support/DictateHub/api-token`
/// with mode 0600 (directory 0700). Provisioned from the hub's
/// `dictated --api-token`; never logged, never written anywhere else.
public struct TokenStore: Sendable {
    public let directory: URL
    public var fileURL: URL { directory.appendingPathComponent("api-token") }

    public static var defaultDirectory: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/DictateHub", isDirectory: true)
    }

    public init(directory: URL = TokenStore.defaultDirectory) { self.directory = directory }

    /// The token, or nil when the file is missing or blank. A file readable
    /// by group or others is tightened to 0600 first.
    public func load() -> String? {
        let path = fileURL.path
        let fm = FileManager.default
        guard fm.fileExists(atPath: path) else { return nil }
        if let perms = (try? fm.attributesOfItem(atPath: path))?[.posixPermissions] as? NSNumber,
           perms.intValue & 0o077 != 0 {
            try? fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: path)
        }
        guard let data = fm.contents(atPath: path),
              let s = String(data: data, encoding: .utf8)?
                .trimmingCharacters(in: .whitespacesAndNewlines),
              !s.isEmpty
        else { return nil }
        return s
    }

    /// Replace the token. Written to a 0600 temp file in the same directory
    /// and renamed over the old one, so the token is never briefly readable.
    public func save(_ token: String) throws {
        let fm = FileManager.default
        try fm.createDirectory(at: directory, withIntermediateDirectories: true,
                               attributes: [.posixPermissions: 0o700])
        let trimmed = token.trimmingCharacters(in: .whitespacesAndNewlines)
        let tmp = directory.appendingPathComponent(".api-token.\(UUID().uuidString)")
        guard fm.createFile(atPath: tmp.path, contents: Data((trimmed + "\n").utf8),
                            attributes: [.posixPermissions: 0o600])
        else { throw CocoaError(.fileWriteUnknown) }
        if rename(tmp.path, fileURL.path) != 0 {
            let err = errno
            try? fm.removeItem(at: tmp)
            throw POSIXError(POSIXErrorCode(rawValue: err) ?? .EIO)
        }
    }
}
