import Foundation
import Testing
@testable import DictateHubCore

@Suite struct StubServerTests {
    static let token = "stub-token-not-a-secret"

    static func json(_ status: Int, _ reason: String, _ body: String, delay: TimeInterval = 0) -> StubServer.Response {
        StubServer.Response(status: status, reason: reason, body: Data(body.utf8), delay: delay)
    }

    /// A stub that checks the bearer token the way the hub does.
    static func hubLike(_ req: StubServer.Recorded) -> StubServer.Response {
        guard req.headers["authorization"] == "Bearer \(token)" else {
            return json(401, "Unauthorized", #"{"code":"unauthorized","message":"missing or invalid bearer token"}"#)
        }
        switch (req.method, req.target.split(separator: "?").first.map(String.init)) {
        case ("POST", "/v1/transcribe"):
            return json(200, "OK", ResponseParsingTests.transcriptJSON)
        case ("GET", "/v1/status"):
            return json(200, "OK", #"{"type":"status","state":"idle","daemon":{"name":"dictated","version":"0.0.0-stub"},"model":{"name":"stub-model","loaded":true,"backend":"cpu"}}"#)
        default:
            return json(404, "Not Found", #"{"code":"not_found","message":"no such route"}"#)
        }
    }

    func client(_ server: StubServer, token: String = StubServerTests.token, timeout: TimeInterval = 30) throws -> HubClient {
        HubClient(builder: try HubRequestBuilder(endpoint: server.endpoint, token: token, timeout: timeout))
    }

    @Test func transcribeRoundTrip() async throws {
        let server = try StubServer(respond: Self.hubLike)
        try await server.start()
        defer { server.stop() }

        let wav = WAV.encode(floatSamples: (0 ..< 1600).map { Float(sin(Double($0) / 8)) * 0.3 })
        let transcript = try await client(server).transcribe(wav: wav, app: "Text Edit & Co")
        #expect(transcript.text == "Hello from the stub.")
        #expect(transcript.route == "type")

        let req = try #require(server.requests.first)
        #expect(req.method == "POST")
        #expect(req.target == "/v1/transcribe?app=Text%20Edit%20%26%20Co")
        #expect(req.headers["content-type"] == "audio/wav")
        #expect(req.headers["authorization"] == "Bearer \(Self.token)")
        #expect(req.headers["host"] == "127.0.0.1:\(server.port)")  // the hub's Host check
        #expect(req.headers["origin"] == nil)  // the hub refuses unknown Origins
        #expect(req.body == wav)
    }

    @Test func statusRoundTrip() async throws {
        let server = try StubServer(respond: Self.hubLike)
        try await server.start()
        defer { server.stop() }
        let status = try await client(server).status()
        #expect(status.summary == "dictated 0.0.0-stub · stub-model (cpu)")
        #expect(server.requests.first?.method == "GET")
    }

    @Test func wrongTokenSurfacesUnauthorized() async throws {
        let server = try StubServer(respond: Self.hubLike)
        try await server.start()
        defer { server.stop() }
        await #expect(throws: HubError.server(status: 401, error: ProtoError(code: "unauthorized",
            message: "missing or invalid bearer token", retryAfterMs: nil))) {
            _ = try await client(server, token: "wrong").transcribe(wav: WAV.encode(samples: [1, 2]), app: nil)
        }
    }

    @Test func busyHubSurfacesProtoError() async throws {
        let server = try StubServer { _ in
            Self.json(409, "Conflict", #"{"code":"busy","message":"a session is already running","retry_after_ms":500}"#)
        }
        try await server.start()
        defer { server.stop() }
        do {
            _ = try await client(server).transcribe(wav: WAV.encode(samples: [1]), app: nil)
            Issue.record("expected an error")
        } catch let e as HubError {
            #expect(e == .server(status: 409, error: ProtoError(code: "busy",
                message: "a session is already running", retryAfterMs: 500)))
            #expect(e.userMessage == "Hub is busy (409): try again")
        }
    }

    @Test func slowHubTimesOut() async throws {
        let server = try StubServer { _ in Self.json(200, "OK", ResponseParsingTests.transcriptJSON, delay: 3) }
        try await server.start()
        defer { server.stop() }
        await #expect(throws: HubError.timeout) {
            _ = try await client(server, timeout: 0.5).transcribe(wav: WAV.encode(samples: [1]), app: nil)
        }
    }

    @Test func closedPortIsUnreachable() async throws {
        // Bind, learn the port, close: nothing listens there now.
        let server = try StubServer(respond: Self.hubLike)
        try await server.start()
        let endpoint = server.endpoint
        server.stop()
        try await Task.sleep(for: .milliseconds(200))
        let c = HubClient(builder: try HubRequestBuilder(endpoint: endpoint, token: Self.token, timeout: 5))
        do {
            _ = try await c.status()
            Issue.record("expected an error")
        } catch let e as HubError {
            guard case .unreachable = e else {
                Issue.record("expected unreachable, got \(e)")
                return
            }
        }
    }

    @Test func emptyClipIsRefusedBeforeSending() async throws {
        let server = try StubServer(respond: Self.hubLike)
        try await server.start()
        defer { server.stop() }
        await #expect(throws: HubError.emptyAudio) {
            _ = try await client(server).transcribe(wav: WAV.encode(samples: []), app: nil)
        }
        #expect(server.requests.isEmpty)
    }
}
