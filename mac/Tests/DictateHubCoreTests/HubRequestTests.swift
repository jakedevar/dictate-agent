import Foundation
import Testing
@testable import DictateHubCore

@Suite struct HubRequestTests {
    let builder = try! HubRequestBuilder(endpoint: "http://127.0.0.1:7313", token: "test-token-123")

    @Test func transcribeRequestShape() throws {
        let wav = WAV.encode(samples: [1, 2, 3])
        let req = builder.transcribeRequest(wav: wav, app: "TextEdit")
        #expect(req.httpMethod == "POST")
        #expect(req.url?.absoluteString == "http://127.0.0.1:7313/v1/transcribe?app=TextEdit")
        #expect(req.value(forHTTPHeaderField: "Content-Type") == "audio/wav")
        #expect(req.value(forHTTPHeaderField: "Authorization") == "Bearer test-token-123")
        #expect(req.value(forHTTPHeaderField: "Accept") == "application/json")
        #expect(req.httpBody == wav)
        #expect(req.timeoutInterval == 30)
    }

    @Test func appNameIsStrictlyEscaped() {
        let req = builder.transcribeRequest(wav: Data(), app: "A&B=C+D #1 Café")
        let url = req.url!.absoluteString
        #expect(url == "http://127.0.0.1:7313/v1/transcribe?app=A%26B%3DC%2BD%20%231%20Caf%C3%A9")
        let items = URLComponents(url: req.url!, resolvingAgainstBaseURL: false)?.queryItems
        #expect(items == [URLQueryItem(name: "app", value: "A&B=C+D #1 Café")])
    }

    @Test func missingAppOmitsTheQuery() {
        #expect(builder.transcribeRequest(wav: Data(), app: nil).url?.absoluteString
            == "http://127.0.0.1:7313/v1/transcribe")
        #expect(builder.transcribeRequest(wav: Data(), app: "").url?.absoluteString
            == "http://127.0.0.1:7313/v1/transcribe")
    }

    @Test func endpointTrailingSlashAndBasePath() throws {
        let b = try HubRequestBuilder(endpoint: " https://hub.example.test:8443/dictate/ ", token: " t \n")
        #expect(b.token == "t")
        #expect(b.transcribeRequest(wav: Data(), app: nil).url?.absoluteString
            == "https://hub.example.test:8443/dictate/v1/transcribe")
        #expect(b.statusRequest().url?.absoluteString == "https://hub.example.test:8443/dictate/v1/status")
    }

    @Test func statusRequestShape() {
        let req = builder.statusRequest()
        #expect(req.httpMethod == "GET")
        #expect(req.url?.absoluteString == "http://127.0.0.1:7313/v1/status")
        #expect(req.value(forHTTPHeaderField: "Authorization") == "Bearer test-token-123")
        #expect(req.httpBody == nil)
        #expect(req.timeoutInterval == 10)
    }

    @Test func rejectsBadEndpointsAndEmptyTokens() {
        #expect(throws: HubError.invalidEndpoint("ftp://x")) {
            try HubRequestBuilder(endpoint: "ftp://x", token: "t")
        }
        #expect(throws: HubError.invalidEndpoint("127.0.0.1:7313")) {
            try HubRequestBuilder(endpoint: "127.0.0.1:7313", token: "t")
        }
        #expect(throws: HubError.invalidEndpoint("")) {
            try HubRequestBuilder(endpoint: "  ", token: "t")
        }
        #expect(throws: HubError.missingToken) {
            try HubRequestBuilder(endpoint: "http://127.0.0.1:7313", token: " \n")
        }
    }
}
