import AppKit
import Foundation
import Testing
@testable import DictateHubCore

@Suite struct TokenStoreTests {
    func tempDir() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("dictatehub-tests-\(UUID().uuidString)/DictateHub", isDirectory: true)
    }

    func mode(_ url: URL) -> Int {
        ((try? FileManager.default.attributesOfItem(atPath: url.path))?[.posixPermissions] as? NSNumber)?.intValue ?? -1
    }

    @Test func missingFileLoadsNil() {
        #expect(TokenStore(directory: tempDir()).load() == nil)
    }

    @Test func saveIs0600InA0700DirAndRoundTrips() throws {
        let store = TokenStore(directory: tempDir())
        defer { try? FileManager.default.removeItem(at: store.directory.deletingLastPathComponent()) }
        try store.save("  synthetic-token\n")
        #expect(store.load() == "synthetic-token")
        #expect(mode(store.fileURL) == 0o600)
        #expect(mode(store.directory) == 0o700)
        try store.save("second")
        #expect(store.load() == "second")
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: store.directory.path)
        #expect(leftovers == ["api-token"])
    }

    @Test func looseModeIsTightenedOnLoad() throws {
        let store = TokenStore(directory: tempDir())
        defer { try? FileManager.default.removeItem(at: store.directory.deletingLastPathComponent()) }
        try store.save("t")
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: store.fileURL.path)
        #expect(store.load() == "t")
        #expect(mode(store.fileURL) == 0o600)
    }

    @Test func blankFileIsNoToken() throws {
        let store = TokenStore(directory: tempDir())
        defer { try? FileManager.default.removeItem(at: store.directory.deletingLastPathComponent()) }
        try store.save("   ")
        #expect(store.load() == nil)
    }
}

@Suite struct SettingsTests {
    func defaults() -> UserDefaults {
        let name = "dictatehub-tests-\(UUID().uuidString)"
        let d = UserDefaults(suiteName: name)!
        d.removePersistentDomain(forName: name)
        return d
    }

    @Test func defaultsAndPersistence() {
        let d = defaults()
        let s = HubSettings(defaults: d)
        #expect(s.endpoint == "http://127.0.0.1:7313")
        #expect(s.hotkey == .rightOption)
        s.endpoint = "http://hub.example.test:7313"
        s.hotkey = .fn
        let again = HubSettings(defaults: d)
        #expect(again.endpoint == "http://hub.example.test:7313")
        #expect(again.hotkey == .fn)
    }

    @Test func blankOrUnknownValuesFallBack() {
        let d = defaults()
        d.set("  ", forKey: HubSettings.endpointKey)
        d.set("caps_lock", forKey: HubSettings.hotkeyKey)
        let s = HubSettings(defaults: d)
        #expect(s.endpoint == HubRequestBuilder.defaultEndpoint)
        #expect(s.hotkey == .default)
    }
}

/// Runs against a private, uniquely named pasteboard, never the user's
/// general pasteboard.
@MainActor
@Suite struct PasteboardTests {
    @Test func swapRestoresEveryItemAndType() throws {
        let pb = NSPasteboard.withUniqueName()
        defer { pb.releaseGlobally() }
        let custom = NSPasteboard.PasteboardType("dev.jakedevar.DictateHub.test")
        let a = NSPasteboardItem()
        a.setString("original text", forType: .string)
        a.setData(Data([1, 2, 3]), forType: custom)
        let b = NSPasteboardItem()
        b.setString("second item", forType: .string)
        pb.clearContents()
        pb.writeObjects([a, b])

        let swap = PasteboardSwap.begin(text: "dictated", on: pb)
        #expect(pb.string(forType: .string) == "dictated")
        #expect(swap.finish())

        let items = try #require(pb.pasteboardItems)
        #expect(items.count == 2)
        #expect(items[0].string(forType: .string) == "original text")
        #expect(items[0].data(forType: custom) == Data([1, 2, 3]))
        #expect(items[1].string(forType: .string) == "second item")
    }

    @Test func restoreSkipsWhenSomeoneElseCopiedMeanwhile() {
        let pb = NSPasteboard.withUniqueName()
        defer { pb.releaseGlobally() }
        pb.clearContents()
        pb.setString("before", forType: .string)
        let swap = PasteboardSwap.begin(text: "dictated", on: pb)
        pb.clearContents()
        pb.setString("user copied this", forType: .string)
        #expect(!swap.finish())
        #expect(pb.string(forType: .string) == "user copied this")
    }

    @Test func emptyPasteboardRestoresEmpty() {
        let pb = NSPasteboard.withUniqueName()
        defer { pb.releaseGlobally() }
        pb.clearContents()
        let swap = PasteboardSwap.begin(text: "dictated", on: pb)
        #expect(swap.snapshot.isEmpty)
        #expect(swap.finish())
        #expect(pb.string(forType: .string) == nil)
    }
}
