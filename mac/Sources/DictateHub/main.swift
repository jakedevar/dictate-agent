import AppKit

// A menu-bar-only app (LSUIElement in Info.plist; .accessory here too, so a
// bare `swift run` behaves the same as the bundle).
let app = NSApplication.shared
let controller = AppController(arguments: CommandLine.arguments)
app.delegate = controller
app.setActivationPolicy(.accessory)
app.run()
