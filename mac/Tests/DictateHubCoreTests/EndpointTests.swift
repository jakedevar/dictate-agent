import Darwin
import Foundation
import Testing
@testable import DictateHubCore

private actor EndpointTransport: HubTransport {
    let failure: HubError?
    let text: String
    var requests: [URLRequest] = []
    init(failure: HubError? = nil, text: String = "local") { self.failure = failure; self.text = text }
    func send(_ request: URLRequest) async throws -> (Data, Int) {
        requests.append(request)
        if let failure { throw failure }
        return (Data("{\"text\":\"\(text)\"}".utf8), 200)
    }
}

@Suite struct EndpointTests {
    let wav = WAV.encode(samples: [1, 2, 3])
    func path() -> String { "/tmp/dhe-\(UUID().uuidString).sock" }
    func socketClient(_ path: String, token: String, timeout: TimeInterval = 2) throws -> HubClient {
        HubClient(builder: try HubRequestBuilder(endpoint: "unix:" + path, token: token, timeout: timeout))
    }
    private func fake(_ transport: EndpointTransport) throws -> HubClient {
        HubClient(builder: try HubRequestBuilder(endpoint: "tunnel", token: "fixture"), transport: transport)
    }

    @Test(arguments: [
        (HubError.notSent(.missingSocket), true), (.notSent(.refused), true), (.notSent(.timeout), true),
        (.http(status: 503), true),
        (.server(status: 503, error: ProtoError(code: "model_unavailable", message: "not ready", retryAfterMs: nil)), true),
        (.http(status: 401), false),
        (.server(status: 401, error: ProtoError(code: "unauthorized", message: "bad token", retryAfterMs: nil)), false),
        (.timeout, false), (.unreachable("post-send connection lost"), false),
        (.badResponse("post-send bad framing"), false), (.http(status: 500), false), (.missingToken, false)
    ])
    func fallbackDecisionAndDispatch(_ error: HubError, _ expected: Bool) async throws {
        #expect(DictationClient.allowsFallback(error) == expected)
        let local = EndpointTransport(failure: error)
        let hub = EndpointTransport(text: "hub")
        let lc = try fake(local), hc = try fake(hub)
        let client = DictationClient(mode: .auto, local: { lc }, hub: { hc })
        do {
            let (transcript, endpoint) = try await client.transcribe(wav: wav, app: "fixture")
            #expect(expected)
            #expect(transcript.text == "hub")
            #expect(endpoint == .fallback)
        } catch let failure as EndpointRequestError {
            #expect(!expected)
            #expect(failure.endpoint == .mac)
            #expect(failure.error == error)
        }
        #expect(await local.requests.count == 1)
        #expect(await hub.requests.count == (expected ? 1 : 0))
    }

    @Test(arguments: [EndpointMode.auto, .mac, .tunnel, .custom])
    func successAndExplicitModes(_ mode: EndpointMode) async throws {
        let local = EndpointTransport(), hub = EndpointTransport(text: "hub"), custom = EndpointTransport(text: "custom")
        let lc = try fake(local), hc = try fake(hub), cc = try fake(custom)
        let client = DictationClient(mode: mode, local: { lc }, hub: { hc }, custom: { cc })
        let (_, used) = try await client.transcribe(wav: wav, app: nil)
        #expect(used == (mode == .tunnel ? .hub : mode == .custom ? .custom : .mac))
        #expect(await local.requests.count == ([.auto, .mac].contains(mode) ? 1 : 0))
        #expect(await hub.requests.count == (mode == .tunnel ? 1 : 0))
        #expect(await custom.requests.count == (mode == .custom ? 1 : 0))
    }

    @Test(arguments: [EndpointMode.mac, .tunnel, .custom])
    func explicitModesNeverFallback(_ mode: EndpointMode) async throws {
        let broken = try fake(EndpointTransport(failure: .notSent(.refused)))
        let unused = EndpointTransport()
        let good = try fake(unused)
        let client = DictationClient(mode: mode, local: { mode == .mac ? broken : good },
            hub: { mode == .tunnel ? broken : good }, custom: { broken })
        await #expect(throws: EndpointRequestError.self) { _ = try await client.transcribe(wav: wav, app: nil) }
        #expect(await unused.requests.isEmpty)
    }

    @Test(arguments: [true, false])
    func autoWithOneSocketDown(_ localUp: Bool) async throws {
        let lp = path(), hp = path()
        let up = try StubServer(socketPath: localUp ? lp : hp) { req in
            let token = localUp ? "local-token" : "hub-token"
            guard req.headers["authorization"] == "Bearer " + token else { return StubServerTests.json(401, "Unauthorized", "{}") }
            return StubServerTests.json(200, "OK", "{\"text\":\"socket success\"}")
        }
        try await up.start()
        defer { up.stop(); unlink(lp); unlink(hp) }
        let lc = try socketClient(lp, token: "local-token"), hc = try socketClient(hp, token: "hub-token")
        let client = DictationClient(mode: .auto, local: { lc }, hub: { hc })
        let (result, used) = try await client.transcribe(wav: wav, app: nil)
        #expect(result.text == "socket success")
        #expect(used == (localUp ? .mac : .fallback))
        #expect(up.requests.count == 1)
        #expect(up.requests.first?.body == wav)
    }

    @Test(arguments: [401, 503, 200])
    func localHTTPDecisionOverRealSockets(_ status: Int) async throws {
        let lp = path(), hp = path()
        let local = try StubServer(socketPath: lp) { _ in
            StubServerTests.json(status, "Fixture", status == 200 ? "{\"text\":\"local\"}" : "{}")
        }
        let hub = try StubServer(socketPath: hp, respond: StubServerTests.hubLike)
        try await local.start(); try await hub.start()
        defer { local.stop(); hub.stop(); unlink(lp); unlink(hp) }
        let lc = try socketClient(lp, token: "local-token"), hc = try socketClient(hp, token: StubServerTests.token)
        let client = DictationClient(mode: .auto, local: { lc }, hub: { hc })
        do {
            let (_, used) = try await client.transcribe(wav: wav, app: nil)
            #expect(status != 401)
            #expect(used == (status == 503 ? .fallback : .mac))
        } catch let failure as EndpointRequestError {
            #expect(status == 401)
            #expect(failure.error == .http(status: 401))
        }
        #expect(local.requests.count == 1)
        #expect(hub.requests.count == (status == 503 ? 1 : 0))
    }

    @Test func acceptedButStalledRequestNeverFallsBack() async throws {
        let lp = path(), hp = path()
        let local = try StubServer(socketPath: lp) { _ in StubServerTests.json(200, "OK", "{}", delay: 1) }
        let hub = try StubServer(socketPath: hp, respond: StubServerTests.hubLike)
        try await local.start(); try await hub.start()
        defer { local.stop(); hub.stop(); unlink(lp); unlink(hp) }
        let lc = try socketClient(lp, token: "local", timeout: 0.15), hc = try socketClient(hp, token: StubServerTests.token)
        let client = DictationClient(mode: .auto, local: { lc }, hub: { hc })
        do {
            _ = try await client.transcribe(wav: wav, app: nil)
            Issue.record("stalled request succeeded")
        } catch let failure as EndpointRequestError { #expect(failure.error == .timeout) }
        #expect(local.requests.count == 1)
        #expect(hub.requests.isEmpty)
    }

    @Test func refusedStaleSocketIsKnownPreSend() async throws {
        let p = path()
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        #expect(fd >= 0)
        defer { close(fd); unlink(p) }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        addr.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        withUnsafeMutableBytes(of: &addr.sun_path) { bytes in
            bytes.copyBytes(from: Array(p.utf8) + [0])
        }
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        #expect(bound == 0) // Bound, but never listen: connect must get ECONNREFUSED.
        await #expect(throws: HubError.notSent(.refused)) { _ = try await socketClient(p, token: "fixture").status() }
    }

    @Test func probeBothEvenWhenLocalRejectsToken() async throws {
        let local = EndpointTransport(failure: .http(status: 401)), hub = EndpointTransport()
        let lc = try fake(local), hc = try fake(hub)
        let lines = await DictationClient(mode: .auto, local: { lc }, hub: { hc }).probe()
        #expect(lines.count == 2)
        #expect(lines[0].contains("401"))
        #expect(lines[0].contains("local token rejected"))
        #expect(lines[1].hasPrefix("Hub OK:"))
        #expect(await local.requests.first?.httpMethod == "GET")
        #expect(await hub.requests.first?.httpMethod == "GET")
        #expect(await local.requests.count == 1)
        #expect(await hub.requests.count == 1)
    }

    @Test func localTokenReadOnlyAndReloadedAfterRotation() async throws {
        let dir = URL(fileURLWithPath: "/tmp/dht-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let local = LocalDictated(directory: dir)
        let missingHub = TokenStore(directory: dir.appendingPathComponent("missing-hub"))
        try Data("first\n".utf8).write(to: local.tokenURL)
        try FileManager.default.setAttributes([.posixPermissions: 0o640], ofItemAtPath: local.tokenURL.path)
        #expect(local.loadToken() == "first")
        let attrs = try FileManager.default.attributesOfItem(atPath: local.tokenURL.path)
        #expect((attrs[.posixPermissions] as? NSNumber)?.intValue == 0o640)
        let server = try StubServer(socketPath: local.socketPath) { req in
            #expect(req.headers["authorization"] == "Bearer second")
            return StubServerTests.json(200, "OK", "{\"text\":\"rotated\"}")
        }
        try FileManager.default.createDirectory(at: dir.appendingPathComponent("run"), withIntermediateDirectories: true)
        try await server.start()
        defer { server.stop() }
        let client = DictationClient(endpoint: "auto", hubTokens: missingHub, localDictated: local)
        try Data("second\n".utf8).write(to: local.tokenURL)
        #expect(try await client.transcribe(wav: wav, app: nil).0.text == "rotated")
        #expect(!FileManager.default.fileExists(atPath: missingHub.directory.path))
    }

    @Test func cancelledUtteranceDoesNotReachFallback() async throws {
        let good = EndpointTransport()
        let hc = try fake(good)
        let client = DictationClient(mode: .auto, local: { throw CancellationError() }, hub: { hc })
        await #expect(throws: CancellationError.self) { _ = try await client.transcribe(wav: wav, app: nil) }
        #expect(await good.requests.isEmpty)
    }

    @Test(arguments: [true, false])
    func missingLocalTokenOnlyFallsBackForMissingSocket(_ socketExists: Bool) async throws {
        let dir = URL(fileURLWithPath: "/tmp/dhm-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir.appendingPathComponent("run"), withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let local = LocalDictated(directory: dir)
        if socketExists { try Data().write(to: URL(fileURLWithPath: local.socketPath)) }
        let client = DictationClient(endpoint: "auto", hubTokens: TokenStore(directory: dir.appendingPathComponent("no-hub")), localDictated: local)
        do {
            _ = try await client.transcribe(wav: wav, app: nil)
            Issue.record("missing tokens succeeded")
        } catch let failure as EndpointRequestError {
            #expect(failure.endpoint == (socketExists ? .mac : .fallback))
            #expect(failure.error == .missingToken)
        }
    }

    @Test func secondClientShellAgainstUnixStub() async throws {
        let dir = URL(fileURLWithPath: "/tmp/dhc-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir.appendingPathComponent("run"), withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        try Data((StubServerTests.token + "\n").utf8).write(to: dir.appendingPathComponent("api-token"))
        let audio = dir.appendingPathComponent("clip with spaces.wav")
        try wav.write(to: audio)
        let server = try StubServer(socketPath: dir.appendingPathComponent("run/api.sock").path, respond: StubServerTests.hubLike)
        try await server.start()
        defer { server.stop() }
        let script = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("scripts/test-local-transcribe.sh")
        let result = try await Task.detached {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/bin/bash")
            process.arguments = [script.path, "--socket-fixture", dir.path, audio.path]
            let out = Pipe()
            process.standardOutput = out
            process.standardError = out
            try process.run()
            let data = out.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            return (process.terminationStatus, data)
        }.value
        #expect(result.0 == 0)
        #expect(try JSONDecoder().decode(Transcript.self, from: result.1).text == "Hello from the stub.")
        #expect(server.requests.count == 1)
        #expect(server.requests.first?.body == wav)
        #expect(server.requests.first?.headers["authorization"] == "Bearer " + StubServerTests.token)
        #expect(server.requests.first?.headers["host"] == "127.0.0.1:7313")
    }
}
