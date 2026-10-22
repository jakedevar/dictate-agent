import Foundation

/// How the app was launched, parsed from its arguments.
///
/// - normal: the menu-bar app.
/// - `--smoke N [--probe] [--onboarding]`: launch check that quits after N s.
/// - `--transcribe-file WAV [--insert] [--app NAME]`: send one audio file
///   through the same encoder, hub client and (with `--insert`) paste path as
///   a live dictation, print one JSON line on stdout, and quit.
/// - `--unregister-login-item`: remove the login item and exit (uninstall).
///
/// `--endpoint URL` and `--token-dir DIR` override the saved endpoint and
/// token, but only in the smoke and file modes, so a check can point at a
/// stub with a throwaway token without touching the user's settings.
public struct LaunchOptions: Equatable, Sendable {
    public enum Mode: Equatable, Sendable {
        case normal
        case smoke(seconds: Double, probe: Bool, onboarding: Bool)
        case transcribeFile(path: String, insert: Bool, app: String?)
        case unregisterLoginItem
    }

    public var mode: Mode
    public var endpointOverride: String?
    public var tokenDirectory: String?

    public init(mode: Mode = .normal, endpointOverride: String? = nil, tokenDirectory: String? = nil) {
        self.mode = mode
        self.endpointOverride = endpointOverride
        self.tokenDirectory = tokenDirectory
    }

    public enum ParseError: Error, Equatable, Sendable {
        case missingValue(String)
        case conflictingModes

        public var message: String {
            switch self {
            case .missingValue(let flag): return "\(flag) needs a value"
            case .conflictingModes: return "--smoke, --transcribe-file and --unregister-login-item are exclusive"
            }
        }
    }

    /// Parse `CommandLine.arguments` (the first element is the executable
    /// and is skipped). Unknown arguments are ignored, as LaunchServices may
    /// add its own.
    public static func parse(_ arguments: [String]) throws -> LaunchOptions {
        let args = Array(arguments.dropFirst())
        func value(after flag: String) throws -> String? {
            guard let i = args.firstIndex(of: flag) else { return nil }
            guard i + 1 < args.count, !args[i + 1].hasPrefix("--") else {
                throw ParseError.missingValue(flag)
            }
            return args[i + 1]
        }
        let smoke = args.contains("--smoke")
        let file = args.contains("--transcribe-file")
        let unregister = args.contains("--unregister-login-item")
        if [smoke, file, unregister].filter({ $0 }).count > 1 { throw ParseError.conflictingModes }

        var options = LaunchOptions()
        if unregister {
            options.mode = .unregisterLoginItem
            return options
        }
        if smoke {
            // The seconds are optional for compatibility with M1 (`--smoke` alone = 3 s).
            let seconds = args.firstIndex(of: "--smoke").flatMap { i in
                i + 1 < args.count ? Double(args[i + 1]) : nil
            } ?? 3
            options.mode = .smoke(seconds: seconds, probe: args.contains("--probe"),
                                  onboarding: args.contains("--onboarding"))
        } else if file {
            guard let path = try value(after: "--transcribe-file") else {
                throw ParseError.missingValue("--transcribe-file")
            }
            options.mode = .transcribeFile(path: path, insert: args.contains("--insert"),
                                           app: try value(after: "--app"))
        }
        if options.mode != .normal {
            options.endpointOverride = try value(after: "--endpoint")
            options.tokenDirectory = try value(after: "--token-dir")
        }
        return options
    }

    public var isSmoke: Bool {
        if case .smoke = mode { return true }
        return false
    }

    public var isFileMode: Bool {
        if case .transcribeFile = mode { return true }
        return false
    }
}

/// The single JSON line file mode prints on stdout. Transcript text goes to
/// stdout only (the PM's end-to-end check reads it); it is never logged.
public struct FileModeResult: Equatable, Sendable, Encodable {
    public var ok: Bool
    public var text: String?
    public var error: String?
    public var audioSeconds: Double?
    public var hubMs: Int?
    public var inserted: Bool
    public var app: String?

    public init(ok: Bool, text: String? = nil, error: String? = nil, audioSeconds: Double? = nil,
                hubMs: Int? = nil, inserted: Bool = false, app: String? = nil) {
        self.ok = ok
        self.text = text
        self.error = error
        self.audioSeconds = audioSeconds
        self.hubMs = hubMs
        self.inserted = inserted
        self.app = app
    }

    enum CodingKeys: String, CodingKey {
        case ok, text, error, inserted, app
        case audioSeconds = "audio_s"
        case hubMs = "hub_ms"
    }

    /// One line, keys sorted, prefixed so a log scraper can find it.
    public var line: String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let json = (try? encoder.encode(self)).flatMap { String(data: $0, encoding: .utf8) } ?? "{\"ok\":false}"
        return "DICTATEHUB_RESULT " + json
    }
}
