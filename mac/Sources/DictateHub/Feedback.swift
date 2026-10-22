import AppKit
import SwiftUI

/// Start/stop/error sounds and the error bubble.
///
/// Sounds are quiet system sounds (`/System/Library/Sounds`), off with the
/// Settings toggle. Errors are the only thing that pops up: a small popover
/// under the menu-bar icon that closes itself. It needs no notification
/// permission (a `UserNotifications` banner would add a third prompt).
@MainActor
final class Feedback {
    static let volume: Float = 0.35
    static let bubbleSeconds: Double = 5

    private let soundsEnabled: () -> Bool
    private var popover: NSPopover?
    private var closeWork: DispatchWorkItem?

    init(soundsEnabled: @escaping () -> Bool) {
        self.soundsEnabled = soundsEnabled
    }

    /// Capture is live: speak now.
    func started() { play("Tink") }
    /// Released: the clip is on its way.
    func stopped() { play("Pop") }
    func failed() { play("Basso") }

    private func play(_ name: String) {
        guard soundsEnabled(),
              let sound = NSSound(named: NSSound.Name(name))?.copy() as? NSSound
        else { return }
        sound.volume = Self.volume
        sound.play()
    }

    /// Show `message` under the status item for a few seconds.
    func showError(_ message: String, below button: NSStatusBarButton?) {
        guard let button, button.window != nil else { return }
        closeWork?.cancel()
        popover?.performClose(nil)
        let p = NSPopover()
        p.behavior = .transient
        p.animates = true
        p.contentViewController = NSHostingController(rootView: ErrorBubbleView(message: message))
        p.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        popover = p
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                self?.popover?.performClose(nil)
                self?.popover = nil
            }
        }
        closeWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.bubbleSeconds, execute: work)
    }
}

private struct ErrorBubbleView: View {
    let message: String

    var body: some View {
        Label(message, systemImage: "exclamationmark.triangle.fill")
            .symbolRenderingMode(.multicolor)
            .font(.callout)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: 320, alignment: .leading)
            .padding(12)
    }
}
