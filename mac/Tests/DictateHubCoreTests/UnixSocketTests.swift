import Foundation
import Testing
@testable import DictateHubCore

@Suite struct UnixSocketTests {
    // Short paths regardless of the harness's TMPDIR (Darwin sun_path = 104).
    func path() -> String { "/tmp/dh-\(UUID().uuidString).sock" }
    func client(_ path: String, token: String = StubServerTests.token, timeout: TimeInterval = 2) throws -> HubClient {
        HubClient(builder: try HubRequestBuilder(endpoint: "unix:" + path, token: token, timeout: timeout))
    }

    @Test func transcriptOverUnixSocket() async throws {
        let p = path()
        let server = try StubServer(socketPath: p, respond: StubServerTests.hubLike)
        try await server.start()
        defer { server.stop(); unlink(p) }
        // Several receive buffers: verifies Content-Length counts bytes.
        let wav = WAV.encode(samples: [Int16](repeating: 123, count: 100_000))
        let answer = try await client(p).transcribe(wav: wav, app: "Text Edit & Co")
        #expect(answer.text == "Hello from the stub.")
        let req = try #require(server.requests.first)
        #expect(req.body == wav)
        #expect(req.target == "/v1/transcribe?app=Text%20Edit%20%26%20Co")
        #expect(req.headers["host"] == "127.0.0.1:7313")
        #expect(req.headers["connection"] == "close")
        #expect(req.headers["origin"] == nil)
        #expect(req.headers["authorization"] == "Bearer \(StubServerTests.token)")
    }

    @Test func statusOverUnixSocket() async throws {
        let p = path()
        let server = try StubServer(socketPath: p, respond: StubServerTests.hubLike)
        try await server.start()
        defer { server.stop(); unlink(p) }
        #expect(try await client(p).status().state == "idle")
    }

    @Test func unauthorizedOverUnixSocket() async throws {
        let p = path()
        let server = try StubServer(socketPath: p, respond: StubServerTests.hubLike)
        try await server.start()
        defer { server.stop(); unlink(p) }
        await #expect(throws: HubError.server(status: 401, error: ProtoError(code: "unauthorized",
            message: "missing or invalid bearer token", retryAfterMs: nil))) {
            _ = try await client(p, token: "wrong").status()
        }
    }

    @Test func stalledSocketTimesOut() async throws {
        let p = path()
        let server = try StubServer(socketPath: p) { _ in
            StubServerTests.json(200, "OK", "{}", delay: 2)
        }
        try await server.start()
        defer { server.stop(); unlink(p) }
        await #expect(throws: HubError.timeout) { _ = try await client(p, timeout: 0.1).status() }
    }

    @Test func missingSocketIsUnreachable() async throws {
        do {
            _ = try await client(path()).status()
            Issue.record("missing socket succeeded")
        } catch let error as HubError {
            guard case .unreachable = error else { Issue.record("expected unreachable"); return }
        }
    }

    @Test func cancellationClosesSocket() async throws {
        let p = path()
        let server = try StubServer(socketPath: p) { _ in StubServerTests.json(200, "OK", "{}", delay: 2) }
        try await server.start()
        defer { server.stop(); unlink(p) }
        let c = try client(p)
        let task = Task { try await c.transport.send(c.builder.statusRequest()) }
        task.cancel()
        await #expect(throws: CancellationError.self) { _ = try await task.value }
    }

    @Test func defaultSelectsUnixAndCustomURLsSelectURLSession() throws {
        let b = try HubRequestBuilder(endpoint: "tunnel", token: "t")
        #expect(b.unixSocketPath == HubRequestBuilder.defaultSocketPath)
        #expect(HubClient(builder: b).transport is UnixSocketTransport)
        let http = try HubRequestBuilder(endpoint: "https://hub.example.test", token: "t")
        #expect(http.unixSocketPath == nil)
        #expect(HubClient(builder: http).transport is URLSessionTransport)
        #expect(throws: HubError.self) { try HubRequestBuilder(endpoint: "unix:relative", token: "t") }
        #expect(throws: HubError.self) { try HubRequestBuilder(endpoint: "tunnel", token: "a\r\nInjected: yes") }
    }

    @Test func partialAndChunkedResponses() throws {
        let header = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n"
        #expect(try UnixHTTP.response(Data((header + "hel").utf8), eof: false) == nil)
        #expect(try UnixHTTP.response(Data((header + "hello").utf8), eof: false)?.0 == Data("hello".utf8))
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhe\r\n3\r\nllo\r\n0\r\n\r\n"
        #expect(try UnixHTTP.response(Data(chunked.utf8), eof: false)?.0 == Data("hello".utf8))
        #expect(try UnixHTTP.response(Data(chunked.dropLast().utf8), eof: false) == nil)
        #expect(throws: HubError.self) { try UnixHTTP.response(Data((header + "he").utf8), eof: true) }
    }

    @Test func oversizedHeaderIsRejected() {
        #expect(throws: HubError.self) {
            try UnixHTTP.response(Data(repeating: 120, count: UnixHTTP.headerLimit + 1), eof: false)
        }
    }

    @Test(arguments: [
        "HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Length: 2\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx!!",
        "garbage\r\n\r\n"
    ])
    func refusesMalformedOrOversizedResponses(_ wire: String) {
        #expect(throws: HubError.self) { try UnixHTTP.response(Data(wire.utf8), eof: true) }
    }
}
