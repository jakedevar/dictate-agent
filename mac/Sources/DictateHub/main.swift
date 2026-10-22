import AppKit
import DictateHubCore

// Entry point. The app is menu-bar only (LSUIElement in the bundle's
// Info.plist; `.accessory` here too, so a bare `swift run` behaves the same as
// the bundle). File mode never becomes active: Cmd+V must reach the app that
// was frontmost, not DictateHub.
let options: LaunchOptions
do {
    options = try LaunchOptions.parse(CommandLine.arguments)
} catch {
    let message = (error as? LaunchOptions.ParseError)?.message ?? "\(error)"
    FileHandle.standardError.write(Data("DictateHub: \(message)\n".utf8))
    exit(2)
}

if options.mode == .unregisterLoginItem {
    exit(MainActor.assumeIsolated { LoginItem.unregisterForUninstall() } ? 0 : 1)
}

let app = NSApplication.shared
let controller = AppController(options: options)
app.delegate = controller
app.setActivationPolicy(options.isFileMode ? .prohibited : .accessory)
app.run()
