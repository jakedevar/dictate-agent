import Darwin
import Foundation

public enum PreSendFailure: String, Equatable, Sendable {
    case missingSocket, refused, timeout
}

public enum EndpointMode: String, CaseIterable, Sendable {
    case auto, mac, tunnel, custom

    public init(endpoint: String) { self = Self(rawValue: endpoint) ?? .custom }
    public var label: String {
        switch self {
        case .auto: return "Auto"
        case .mac: return "Mac only"
        case .tunnel: return "Hub only"
        case .custom: return "Custom URL"
        }
    }
}

public enum EndpointUse: String, Equatable, Sendable {
    case mac, hub, fallback, custom
    public var label: String {
        switch self {
        case .mac: return "Mac (local)"
        case .hub: return "Hub"
        case .fallback: return "Hub (fallback)"
        case .custom: return "Custom URL"
        }
    }
    public var statusLine: String { "Using: " + label }
    public var logName: String {
        switch self {
        case .mac: return "local"
        case .hub: return "hub"
        case .fallback: return "hub-fallback"
        case .custom: return "custom"
        }
    }
}

/// Owns neither the local token nor its permissions: read it anew for each
/// request so dictated's rotation takes effect without restarting the app.
public struct LocalDictated: Sendable {
    public let directory: URL
    public static var defaultDirectory: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/dictated", isDirectory: true)
    }
    public init(directory: URL = Self.defaultDirectory) { self.directory = directory }
    public var socketPath: String { directory.appendingPathComponent("run/api.sock").path }
    public var tokenURL: URL { directory.appendingPathComponent("api-token") }
    public func loadToken() -> String? {
        guard let token = try? String(contentsOf: tokenURL, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines), !token.isEmpty else { return nil }
        return token
    }
}

public struct EndpointRequestError: Error, Sendable {
    public let endpoint: EndpointUse
    public let error: HubError
    public var userMessage: String {
        if endpoint == .mac, error == .missingToken {
            return "Mac (local): No dictated API token; start dictated on this Mac"
        }
        if endpoint == .mac {
            switch error {
            case .http(status: 401), .server(status: 401, error: _):
                return "Mac (local): local token rejected (401); check dictated/api-token"
            default: break
            }
        }
        return endpoint.label + ": " + error.userMessage
    }
}

/// Endpoint selection is per utterance. Only known pre-send failures and an
/// explicit HTTP 503 rejection allow a second POST. Unknown errors fail closed.
public struct DictationClient: Sendable {
    public typealias ClientFactory = @Sendable () throws -> HubClient
    let mode: EndpointMode
    let local: ClientFactory
    let hub: ClientFactory
    let custom: ClientFactory

    public init(endpoint: String, hubTokens: TokenStore = TokenStore(),
                localDictated: LocalDictated = LocalDictated()) {
        self.init(mode: EndpointMode(endpoint: endpoint), local: {
            guard let token = localDictated.loadToken() else {
                // An uninstalled local daemon must not require a local token
                // just to reach the hub. An existing socket with no token is
                // a configuration error, not permission to send audio away.
                var info = stat()
                if lstat(localDictated.socketPath, &info) != 0, errno == ENOENT {
                    throw HubError.notSent(.missingSocket)
                }
                throw HubError.missingToken
            }
            return HubClient(builder: try HubRequestBuilder(endpoint: "unix:" + localDictated.socketPath, token: token))
        }, hub: {
            guard let token = hubTokens.load() else { throw HubError.missingToken }
            return HubClient(builder: try HubRequestBuilder(endpoint: "tunnel", token: token))
        }, custom: {
            guard let token = hubTokens.load() else { throw HubError.missingToken }
            return HubClient(builder: try HubRequestBuilder(endpoint: endpoint, token: token))
        })
    }

    public static func tokenPresent(endpoint: String, hubTokens: TokenStore) -> Bool {
        switch EndpointMode(endpoint: endpoint) {
        case .auto: return LocalDictated().loadToken() != nil || hubTokens.load() != nil
        case .mac: return LocalDictated().loadToken() != nil
        case .tunnel, .custom: return hubTokens.load() != nil
        }
    }

    public init(mode: EndpointMode, local: @escaping ClientFactory,
                hub: @escaping ClientFactory, custom: @escaping ClientFactory = { throw HubError.invalidEndpoint("custom") }) {
        self.mode = mode
        self.local = local
        self.hub = hub
        self.custom = custom
    }

    public static func allowsFallback(_ error: HubError) -> Bool {
        switch error {
        case .notSent: return true
        case .http(status: 503), .server(status: 503, error: _): return true
        default: return false
        }
    }

    public func transcribe(wav: Data, app: String?) async throws -> (Transcript, EndpointUse) {
        guard wav.count > WAV.headerSize else { throw HubError.emptyAudio }
        switch mode {
        case .auto:
            do { return try await attempt(local, endpoint: .mac, wav: wav, app: app) }
            catch let failure as EndpointRequestError where Self.allowsFallback(failure.error) {
                try Task.checkCancellation()
                return try await attempt(hub, endpoint: .fallback, wav: wav, app: app)
            }
        case .mac: return try await attempt(local, endpoint: .mac, wav: wav, app: app)
        case .tunnel: return try await attempt(hub, endpoint: .hub, wav: wav, app: app)
        case .custom: return try await attempt(custom, endpoint: .custom, wav: wav, app: app)
        }
    }

    private func attempt(_ factory: ClientFactory, endpoint: EndpointUse, wav: Data, app: String?) async throws -> (Transcript, EndpointUse) {
        do { return (try await factory().transcribe(wav: wav, app: app), endpoint) }
        catch is CancellationError { throw CancellationError() }
        catch { throw EndpointRequestError(endpoint: endpoint, error: hubError(fromTransport: error)) }
    }

    /// Probe both independently, even when the preferred endpoint succeeds or
    /// rejects its token. GET probes never change the last dictation endpoint.
    public func probe() async -> [String] {
        if mode == .custom { return [await probeOne(custom, endpoint: .custom)] }
        async let mac = probeOne(local, endpoint: .mac)
        async let hubResult = probeOne(hub, endpoint: .hub)
        return await [mac, hubResult]
    }

    private func probeOne(_ factory: ClientFactory, endpoint: EndpointUse) async -> String {
        let label = endpoint == .mac ? "Mac" : endpoint == .hub ? "Hub" : "Custom"
        do { return label + " OK: " + (try await factory().status()).summary }
        catch { return EndpointRequestError(endpoint: endpoint, error: hubError(fromTransport: error)).userMessage }
    }
}
