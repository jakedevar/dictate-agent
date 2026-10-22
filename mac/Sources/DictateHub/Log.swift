import Foundation
import os

/// Unified log (subsystem dev.jakedevar.DictateHub) plus stderr, so a launch
/// with `open --stderr FILE` can be checked headless. Never log transcript
/// text or the token.
enum Log {
    private static let logger = Logger(subsystem: "dev.jakedevar.DictateHub", category: "app")

    static func info(_ message: String) {
        logger.info("\(message, privacy: .public)")
        FileHandle.standardError.write(Data("DictateHub: \(message)\n".utf8))
    }

    static func error(_ message: String) {
        logger.error("\(message, privacy: .public)")
        FileHandle.standardError.write(Data("DictateHub: error: \(message)\n".utf8))
    }
}
