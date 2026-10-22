import Foundation
import Testing
@testable import DictateHubCore

@Suite struct ResponseParsingTests {
    // Shape of the S33 `transcript` result (docs/protocol.md "Network API (S33)").
    static let transcriptJSON = """
    {"type":"transcript","text":"Hello from the stub.","route":"type",
     "timings":{"stt_ms":120,"format_ms":40,"total_ms":180},
     "injection":{"status":"delivered"},"word_count":4,"model":"large-v3-turbo",
     "some_future_field":{"x":1}}
    """

    @Test func decodesTranscriptIgnoringUnknownFields() throws {
        let t = try decodeHubResponse(Transcript.self, status: 200, body: Data(Self.transcriptJSON.utf8))
        #expect(t == Transcript(text: "Hello from the stub.", route: "type", wordCount: 4, model: "large-v3-turbo"))
    }

    @Test func minimalTranscript() throws {
        let t = try decodeHubResponse(Transcript.self, status: 200,
                                      body: Data(#"{"text":"","injection":{"status":"delivered"}}"#.utf8))
        #expect(t.text == "")
        #expect(t.route == nil)
    }

    @Test func wrongResultTypeIsABadResponse() {
        #expect(throws: HubError.badResponse("could not decode Transcript")) {
            try decodeHubResponse(Transcript.self, status: 200, body: Data(#"{"type":"status","text":"x"}"#.utf8))
        }
        #expect(throws: HubError.badResponse("could not decode Transcript")) {
            try decodeHubResponse(Transcript.self, status: 200, body: Data("not json".utf8))
        }
    }

    @Test func protoErrorBodyMapsToServerError() {
        let body = Data(#"{"code":"unauthorized","message":"missing or invalid bearer token"}"#.utf8)
        let expected = HubError.server(status: 401, error: ProtoError(code: "unauthorized",
            message: "missing or invalid bearer token", retryAfterMs: nil))
        #expect(throws: expected) { try decodeHubResponse(Transcript.self, status: 401, body: body) }
        #expect(expected.userMessage.contains("401"))
    }

    @Test func rateLimitedCarriesRetryAfter() {
        let body = Data(#"{"code":"rate_limited","message":"slow down","detail":{"retry_after_s":3},"retry_after_ms":3000}"#.utf8)
        #expect(throws: HubError.server(status: 429, error: ProtoError(code: "rate_limited",
            message: "slow down", retryAfterMs: 3000))) {
            try decodeHubResponse(Transcript.self, status: 429, body: body)
        }
    }

    @Test func nonJSONErrorMapsToHTTPStatus() {
        #expect(throws: HubError.http(status: 502)) {
            try decodeHubResponse(Transcript.self, status: 502, body: Data("<html>bad gateway</html>".utf8))
        }
        #expect(throws: HubError.http(status: 500)) {
            try decodeHubResponse(Transcript.self, status: 500, body: Data())
        }
    }

    @Test func statusSummary() throws {
        let body = Data("""
        {"type":"status","state":"idle","daemon":{"name":"dictated","version":"0.9.0","protocol_version":1},
         "model":{"name":"large-v3-turbo","loaded":true,"backend":"cuda"},"capabilities":{}}
        """.utf8)
        let s = try decodeHubResponse(HubStatus.self, status: 200, body: body)
        #expect(s.summary == "dictated 0.9.0 · large-v3-turbo (cuda)")
        #expect(try decodeHubResponse(HubStatus.self, status: 200, body: Data("{}".utf8)).summary == "connected")
    }

    @Test func transportErrorsMapToUserStates() {
        #expect(hubError(fromTransport: URLError(.timedOut)) == .timeout)
        if case .unreachable = hubError(fromTransport: URLError(.cannotConnectToHost)) {} else {
            Issue.record("connection refused should be unreachable")
        }
        #expect(hubError(fromTransport: HubError.missingToken) == .missingToken)
    }

    @Test func userMessagesNeverEchoAToken() {
        let messages = [
            HubError.missingToken, .timeout, .emptyAudio, .http(status: 418),
            .server(status: 409, error: ProtoError(code: "busy", message: "a session is running", retryAfterMs: 500)),
        ].map(\.userMessage)
        #expect(messages.allSatisfy { !$0.isEmpty && !$0.contains("Bearer") })
    }
}
