import Foundation
import ServiceManagement

/// "Open at login" through `SMAppService.mainApp` (macOS 13+): no helper
/// app, no launch agent plist. The system owns the state; the user can also
/// switch it in System Settings → General → Login Items.
@MainActor
enum LoginItem {
    static var status: SMAppService.Status { SMAppService.mainApp.status }

    static var isEnabled: Bool { status == .enabled }

    static var statusText: String {
        switch status {
        case .enabled: return "enabled"
        case .notRegistered: return "off"
        case .requiresApproval: return "needs approval in System Settings → General → Login Items"
        case .notFound: return "not found"
        @unknown default: return "unknown"
        }
    }

    /// Only an installed copy registers itself: a build-dir or smoke-test
    /// launch must never leave a login item pointing at a scratch bundle.
    static var isInstalledLocation: Bool {
        let path = Bundle.main.bundleURL.standardizedFileURL.path
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return path == "\(home)/Applications/DictateHub.app" || path == "/Applications/DictateHub.app"
    }

    static func set(_ on: Bool) throws {
        if on {
            try SMAppService.mainApp.register()
        } else {
            try SMAppService.mainApp.unregister()
        }
    }

    /// `--unregister-login-item` (uninstall): remove the login item before
    /// the bundle is deleted, so no stale entry is left in Login Items.
    static func unregisterForUninstall() -> Bool {
        let before = statusText
        do {
            if status != .notRegistered { try SMAppService.mainApp.unregister() }
            print("login item: \(before) -> \(statusText)")
            return true
        } catch {
            print("login item: unregister failed: \(error.localizedDescription) (status \(statusText))")
            return status == .notRegistered || status == .notFound
        }
    }

    static func openSystemSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }
}
