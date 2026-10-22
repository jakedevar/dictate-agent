import AppKit

/// A copy of every item on a pasteboard, every type of each, so the user's
/// clipboard survives the paste-at-cursor round trip (text, images, files,
/// rich text: whatever was there).
public struct PasteboardSnapshot: @unchecked Sendable {
    // Holds only value types (Data); @unchecked because NSPasteboard.PasteboardType
    // is a struct wrapper over String that is not declared Sendable in every SDK.
    public let items: [[(NSPasteboard.PasteboardType, Data)]]

    public static func capture(_ pasteboard: NSPasteboard) -> PasteboardSnapshot {
        let items = (pasteboard.pasteboardItems ?? []).map { item in
            item.types.compactMap { type in item.data(forType: type).map { (type, $0) } }
        }
        return PasteboardSnapshot(items: items)
    }

    public var isEmpty: Bool { items.allSatisfy(\.isEmpty) }

    /// Put the snapshot back. Returns the pasteboard's new change count.
    @discardableResult
    public func restore(to pasteboard: NSPasteboard) -> Int {
        pasteboard.clearContents()
        let objects: [NSPasteboardItem] = items.compactMap { entries in
            guard !entries.isEmpty else { return nil }
            let item = NSPasteboardItem()
            for (type, data) in entries { item.setData(data, forType: type) }
            return item
        }
        if !objects.isEmpty { pasteboard.writeObjects(objects) }
        return pasteboard.changeCount
    }
}

/// The pasteboard half of paste-at-cursor: snapshot, set the text, and later
/// restore — but only if nothing else wrote to the pasteboard in between, so
/// a copy the user makes during the restore delay is never clobbered.
public struct PasteboardSwap {
    public let pasteboard: NSPasteboard
    public let snapshot: PasteboardSnapshot
    /// The change count right after our text was written.
    public let ourChangeCount: Int

    /// Snapshot `pasteboard` and replace its contents with `text`.
    public static func begin(text: String, on pasteboard: NSPasteboard) -> PasteboardSwap {
        let snap = PasteboardSnapshot.capture(pasteboard)
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
        // Ask well-behaved clipboard managers not to record the transient text.
        pasteboard.setString("", forType: NSPasteboard.PasteboardType("org.nspasteboard.TransientType"))
        return PasteboardSwap(pasteboard: pasteboard, snapshot: snap, ourChangeCount: pasteboard.changeCount)
    }

    /// Restore the snapshot. Returns false (and leaves the pasteboard alone)
    /// when someone else changed it after we did.
    @discardableResult
    public func finish() -> Bool {
        guard pasteboard.changeCount == ourChangeCount else { return false }
        snapshot.restore(to: pasteboard)
        return true
    }
}
