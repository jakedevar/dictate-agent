import Foundation

/// Where the installed copy lives, and the two decisions that depend on it
/// (#3807). Pure logic, so it is unit-tested without AppKit or ServiceManagement.
public enum InstallLocation {
    /// The one installed path. `remote.sh install` puts the bundle here (no sudo:
    /// /Applications is writable by an admin user), so Launchpad, Spotlight and
    /// Finder all find it.
    public static let installedPath = "/Applications/DictateHub.app"

    /// The per-user location earlier installs used; `install.sh` migrates away
    /// from it and the app no longer counts it as installed.
    public static func legacyPath(home: String) -> String {
        home + "/Applications/DictateHub.app"
    }

    public static func isInstalled(bundlePath: String) -> Bool {
        normalized(bundlePath) == installedPath
    }

    public static func isLegacy(bundlePath: String, home: String) -> Bool {
        normalized(bundlePath) == legacyPath(home: home)
    }

    /// Standardize and drop a trailing slash. Symlinks are not resolved: a
    /// bundle opened through a link is not the installed copy.
    static func normalized(_ path: String) -> String {
        var p = (path as NSString).standardizingPath
        while p.count > 1 && p.hasSuffix("/") { p.removeLast() }
        return p
    }
}

/// What opening the app again (Finder, Launchpad, Spotlight, `open`) shows
/// while it is already running. An LSUIElement app has no Dock icon and no
/// window, so without this the open looks like nothing happened.
public enum ReopenWindow: Equatable, Sendable {
    case onboarding
    case settings

    /// The setup window while anything dictation needs is missing or the first
    /// run has not been finished; Settings once everything is granted.
    /// `missing` is the permission diagnosis (it also covers Input Monitoring
    /// for a chord hotkey, which the checklist does not).
    public static func decide(checklist: OnboardingChecklist, onboardingDone: Bool,
                              missing: [MissingPermission]) -> ReopenWindow {
        if checklist.shouldOpenAtLaunch(firstRun: !onboardingDone) || !missing.isEmpty {
            return .onboarding
        }
        return .settings
    }
}

/// Whether the login item must be re-registered at launch so that it points at
/// the running bundle. SMAppService records the bundle it registered; a copy
/// moved from ~/Applications to /Applications would otherwise leave an entry
/// for the old path. The path last registered is remembered in the settings
/// (`loginItemRegisteredPath`) because ServiceManagement does not expose it.
public enum LoginItemPlan: Equatable, Sendable {
    /// Nothing to do (not the installed copy, a smoke/file run, switched off,
    /// or already registered from this path).
    case keep
    /// Unregister, then register from the running bundle, and remember the path.
    case reregister

    /// - Parameters:
    ///   - isNormalLaunch: false for `--smoke` and file-mode runs, which never
    ///     touch the login item.
    ///   - enabled: `SMAppService.mainApp.status == .enabled` right now.
    public static func decide(bundlePath: String, registeredPath: String?, enabled: Bool,
                              isNormalLaunch: Bool) -> LoginItemPlan {
        guard isNormalLaunch, enabled, InstallLocation.isInstalled(bundlePath: bundlePath) else { return .keep }
        if let registeredPath, InstallLocation.normalized(registeredPath) == InstallLocation.normalized(bundlePath) {
            return .keep
        }
        return .reregister
    }
}
