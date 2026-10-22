import Foundation

/// The hub's answer to `POST /v1/transcribe`: the S33 `transcript` result.
/// Unknown fields are ignored (protocol rule: clients tolerate additions).
public struct Transcript: Decodable, Equatable, Sendable {
    public let text: String
    public let route: String?
    public let wordCount: Int?
    public let model: String?

    enum CodingKeys: String, CodingKey {
        case type, text, route, model
        case wordCount = "word_count"
    }

    public init(text: String, route: String? = nil, wordCount: Int? = nil, model: String? = nil) {
        self.text = text
        self.route = route
        self.wordCount = wordCount
        self.model = model
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let type = try c.decodeIfPresent(String.self, forKey: .type), type != "transcript" {
            throw DecodingError.dataCorruptedError(
                forKey: .type, in: c, debugDescription: "expected a transcript result, got \(type)")
        }
        text = try c.decode(String.self, forKey: .text)
        route = try c.decodeIfPresent(String.self, forKey: .route)
        wordCount = try c.decodeIfPresent(Int.self, forKey: .wordCount)
        model = try c.decodeIfPresent(String.self, forKey: .model)
    }
}

/// The hub's answer to `GET /v1/status` (only what the menu shows).
public struct HubStatus: Decodable, Equatable, Sendable {
    public struct Daemon: Decodable, Equatable, Sendable {
        public let name: String?
        public let version: String?
    }
    public struct Model: Decodable, Equatable, Sendable {
        public let name: String?
        public let loaded: Bool?
        public let backend: String?
    }
    public let state: String?
    public let daemon: Daemon?
    public let model: Model?

    /// One line for the menu: "dictated 0.9.0 · large-v3-turbo (cuda)".
    public var summary: String {
        var parts: [String] = []
        if let d = daemon {
            parts.append([d.name, d.version].compactMap { $0 }.joined(separator: " "))
        }
        if let m = model, let name = m.name {
            parts.append(m.backend.map { "\(name) (\($0))" } ?? name)
        }
        let line = parts.filter { !$0.isEmpty }.joined(separator: " · ")
        return line.isEmpty ? "connected" : line
    }
}

/// A protocol error object (`docs/protocol.md` §10).
public struct ProtoError: Decodable, Equatable, Sendable {
    public let code: String
    public let message: String
    public let retryAfterMs: Int?

    enum CodingKeys: String, CodingKey {
        case code, message
        case retryAfterMs = "retry_after_ms"
    }
}

/// Everything that can go wrong between "release the hotkey" and "text".
public enum HubError: Error, Equatable, Sendable {
    /// No token file / token is empty.
    case missingToken
    /// The endpoint is not the secure tunnel, a valid UDS path or an HTTP(S) URL.
    case invalidEndpoint(String)
    /// The clip was empty.
    case emptyAudio
    /// The hub answered with a protocol error object.
    case server(status: Int, error: ProtoError)
    /// The hub answered with a non-2xx status and no parseable error object.
    case http(status: Int)
    /// The request did not complete in time.
    case timeout
    /// The hub could not be reached (connection refused, tunnel down, …).
    case unreachable(String)
    /// The body of a 2xx answer was not the expected shape.
    case badResponse(String)

    /// Short text for the menu's status line. Never contains transcript text.
    public var userMessage: String {
        switch self {
        case .missingToken:
            return "No API token: save it in Settings"
        case .invalidEndpoint(let s):
            return "Invalid hub endpoint: \(s)"
        case .emptyAudio:
            return "No audio was captured"
        case .server(let status, let e):
            switch e.code {
            case "unauthorized": return "Hub rejected the token (401): update it in Settings"
            case "rate_limited": return "Hub rate limit hit (429): try again shortly"
            case "busy": return "Hub is busy (409): try again"
            case "payload_too_large": return "Clip too large for the hub (413)"
            case "model_unavailable": return "Hub model unavailable (503)"
            default: return "Hub error \(status) \(e.code): \(e.message)"
            }
        case .http(let status):
            return "Hub answered HTTP \(status)"
        case .timeout:
            return "Hub did not answer in time"
        case .unreachable(let why):
            return "Hub unreachable: \(why)"
        case .badResponse(let why):
            return "Unexpected hub response: \(why)"
        }
    }
}

/// Builds requests for the S33 network API and interprets the answers.
/// Pure: no I/O, so it is tested directly.
public struct HubRequestBuilder: Sendable {
    public static let defaultEndpoint = "tunnel"
    public static let legacyEndpoint = "http://127.0.0.1:7313"
    public static var defaultSocketPath: String {
        TokenStore.defaultDirectory.appendingPathComponent("run/hub.sock").path
    }
    public static let defaultTimeout: TimeInterval = 30

    public let endpoint: URL
    public let unixSocketPath: String?
    public let token: String
    public let timeout: TimeInterval

    public init(endpoint: String, token: String, timeout: TimeInterval = HubRequestBuilder.defaultTimeout) throws {
        let trimmed = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        let socket: String?
        let httpEndpoint: String
        if trimmed == Self.defaultEndpoint || trimmed.hasPrefix("unix:") {
            socket = trimmed == Self.defaultEndpoint ? Self.defaultSocketPath : String(trimmed.dropFirst(5))
            guard let socket, socket.hasPrefix("/"), socket.utf8.count < 104,
                  !socket.contains("\0"), !socket.contains("\n"), !socket.contains("\r")
            else { throw HubError.invalidEndpoint(trimmed) }
            httpEndpoint = Self.legacyEndpoint
        } else {
            socket = nil
            httpEndpoint = trimmed
        }
        guard let url = URL(string: httpEndpoint),
              let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https",
              url.host != nil
        else { throw HubError.invalidEndpoint(trimmed) }
        self.unixSocketPath = socket
        let tok = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !tok.isEmpty else { throw HubError.missingToken }
        guard !tok.contains("\r"), !tok.contains("\n") else { throw HubError.missingToken }
        self.endpoint = url
        self.token = tok
        self.timeout = timeout
    }

    /// Characters left unescaped in a query value: RFC 3986 unreserved only,
    /// so `&`, `=`, `+`, `#` and spaces in an app name are always escaped.
    static let queryValueAllowed: CharacterSet = {
        CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
    }()

    func url(path: String, query: [(String, String)] = []) -> URL {
        var base = endpoint.absoluteString
        while base.hasSuffix("/") { base.removeLast() }
        var s = base + path
        if !query.isEmpty {
            s += "?" + query.map { k, v in
                let ek = k.addingPercentEncoding(withAllowedCharacters: Self.queryValueAllowed) ?? k
                let ev = v.addingPercentEncoding(withAllowedCharacters: Self.queryValueAllowed) ?? v
                return "\(ek)=\(ev)"
            }.joined(separator: "&")
        }
        // Built from a validated URL plus escaped components, so it parses.
        return URL(string: s)!
    }

    /// `POST {endpoint}/v1/transcribe?app=<app>` with the WAV as the body.
    public func transcribeRequest(wav: Data, app: String?) -> URLRequest {
        var query: [(String, String)] = []
        if let app, !app.isEmpty { query.append(("app", app)) }
        var req = URLRequest(url: url(path: "/v1/transcribe", query: query))
        req.httpMethod = "POST"
        req.timeoutInterval = timeout
        req.setValue("audio/wav", forHTTPHeaderField: "Content-Type")
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        req.httpBody = wav
        return req
    }

    /// `GET {endpoint}/v1/status`, the "Test connection" probe.
    public func statusRequest() -> URLRequest {
        var req = URLRequest(url: url(path: "/v1/status"))
        req.httpMethod = "GET"
        req.timeoutInterval = min(timeout, 10)
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        return req
    }
}

/// Interpret an HTTP answer: 2xx decodes `T`; anything else becomes a
/// `HubError`, preferring the protocol error object when the body has one.
public func decodeHubResponse<T: Decodable>(_ type: T.Type, status: Int, body: Data) throws -> T {
    if (200..<300).contains(status) {
        do {
            return try JSONDecoder().decode(T.self, from: body)
        } catch {
            // A 2xx carrying an error object is still an error.
            if let e = try? JSONDecoder().decode(ProtoError.self, from: body) {
                throw HubError.server(status: status, error: e)
            }
            throw HubError.badResponse("could not decode \(T.self)")
        }
    }
    if let e = try? JSONDecoder().decode(ProtoError.self, from: body) {
        throw HubError.server(status: status, error: e)
    }
    throw HubError.http(status: status)
}

/// Map a transport-level failure to a `HubError`.
public func hubError(fromTransport error: Error) -> HubError {
    if let e = error as? HubError { return e }
    if let u = error as? URLError {
        switch u.code {
        case .timedOut: return .timeout
        case .cannotConnectToHost: return .unreachable("connection refused (is the tunnel up?)")
        case .cannotFindHost, .dnsLookupFailed: return .unreachable("unknown host")
        case .notConnectedToInternet, .networkConnectionLost: return .unreachable("network down")
        default: return .unreachable(u.localizedDescription)
        }
    }
    return .unreachable(error.localizedDescription)
}

/// Sends requests. A seam so tests can run against a stub server or a fake.
public protocol HubTransport: Sendable {
    func send(_ request: URLRequest) async throws -> (Data, Int)
}

/// The real transport: an ephemeral `URLSession` (no cookies, no cache, no
/// credential storage) with the request timeout also applied to the whole
/// resource, so a stalled tunnel cannot hang the app past it.
public struct URLSessionTransport: HubTransport {
    let session: URLSession

    public init(timeout: TimeInterval = HubRequestBuilder.defaultTimeout) {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = timeout
        config.timeoutIntervalForResource = timeout
        config.httpCookieStorage = nil
        config.urlCache = nil
        config.urlCredentialStorage = nil
        config.waitsForConnectivity = false
        session = URLSession(configuration: config)
    }

    public func send(_ request: URLRequest) async throws -> (Data, Int) {
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw HubError.badResponse("not an HTTP response")
        }
        return (data, http.statusCode)
    }
}

/// The hub client: build, send, interpret.
public struct HubClient: Sendable {
    public let builder: HubRequestBuilder
    public let transport: HubTransport

    public init(builder: HubRequestBuilder, transport: HubTransport? = nil) {
        self.builder = builder
        if let transport { self.transport = transport }
        else if let path = builder.unixSocketPath { self.transport = UnixSocketTransport(path: path) }
        else { self.transport = URLSessionTransport(timeout: builder.timeout) }
    }

    public func transcribe(wav: Data, app: String?) async throws -> Transcript {
        guard wav.count > WAV.headerSize else { throw HubError.emptyAudio }
        let (data, status) = try await send(builder.transcribeRequest(wav: wav, app: app))
        return try decodeHubResponse(Transcript.self, status: status, body: data)
    }

    public func status() async throws -> HubStatus {
        let (data, status) = try await send(builder.statusRequest())
        return try decodeHubResponse(HubStatus.self, status: status, body: data)
    }

    private func send(_ request: URLRequest) async throws -> (Data, Int) {
        do {
            return try await transport.send(request)
        } catch {
            throw hubError(fromTransport: error)
        }
    }
}
