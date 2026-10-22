import Foundation
import Network

/// HTTP/1.1 over the SSH tunnel's private Unix socket. Each request owns one
/// connection and one wall-clock deadline; no cookies, redirects or retries.
public struct UnixSocketTransport: HubTransport {
    public let path: String
    public init(path: String) { self.path = path }

    public func send(_ request: URLRequest) async throws -> (Data, Int) {
        let exchange = UnixExchange(path: path)
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                exchange.start(request, continuation: continuation)
            }
        } onCancel: {
            exchange.cancel()
        }
    }
}

// All mutable state belongs to queue, including cancellation before start.
private final class UnixExchange: @unchecked Sendable {
    let queue = DispatchQueue(label: "DictateHub.unix-http")
    let connection: NWConnection
    var continuation: CheckedContinuation<(Data, Int), any Error>?
    var timer: DispatchWorkItem?
    var data = Data()
    var finished = false
    var cancelled = false

    init(path: String) {
        connection = NWConnection(to: .unix(path: path), using: .tcp)
    }

    func start(_ request: URLRequest, continuation: CheckedContinuation<(Data, Int), any Error>) {
        queue.async {
            self.continuation = continuation
            if self.cancelled { self.finish(.failure(CancellationError())); return }
            let wire: Data
            do { wire = try UnixHTTP.request(request) }
            catch { self.finish(.failure(error)); return }
            let timer = DispatchWorkItem { self.finish(.failure(HubError.timeout)) }
            self.timer = timer
            self.queue.asyncAfter(deadline: .now() + max(0.001, request.timeoutInterval), execute: timer)
            self.connection.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    self.connection.send(content: wire, completion: .contentProcessed { error in
                        if error != nil { self.finish(.failure(HubError.unreachable("tunnel send failed"))) }
                        else { self.receive() }
                    })
                case .failed, .waiting:
                    self.finish(.failure(HubError.unreachable("secure tunnel unavailable")))
                default: break
                }
            }
            self.connection.start(queue: self.queue)
        }
    }

    func cancel() {
        queue.async {
            self.cancelled = true
            if self.continuation != nil { self.finish(.failure(CancellationError())) }
        }
    }

    func receive() {
        guard !finished else { return }
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65_536) { bytes, _, eof, error in
            guard !self.finished else { return }
            if let bytes { self.data.append(bytes) }
            do {
                if let response = try UnixHTTP.response(self.data, eof: eof) {
                    self.finish(.success(response))
                } else if error != nil {
                    self.finish(.failure(HubError.unreachable("tunnel receive failed")))
                } else { self.receive() }
            } catch { self.finish(.failure(error)) }
        }
    }

    func finish(_ result: Result<(Data, Int), any Error>) {
        guard !finished else { return }
        finished = true
        timer?.cancel()
        timer = nil
        connection.stateUpdateHandler = nil
        connection.cancel()
        let saved = continuation
        continuation = nil
        saved?.resume(with: result)
    }
}

/// Bounded parser for one HTTP response; never includes response/request bytes
/// in errors, since they may contain transcripts or credentials.
enum UnixHTTP {
    static let headerLimit = 65_536
    static let bodyLimit = 8 * 1_024 * 1_024
    static let crlf = Data("\r\n".utf8)
    static let separator = Data("\r\n\r\n".utf8)
    static func malformed() -> HubError { .badResponse("invalid HTTP framing from secure tunnel") }

    static func request(_ request: URLRequest) throws -> Data {
        guard let url = request.url, let parts = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else { throw malformed() }
        let target = (parts.percentEncodedPath.isEmpty ? "/" : parts.percentEncodedPath)
            + (parts.percentEncodedQuery.map { "?" + $0 } ?? "")
        let body = request.httpBody ?? Data()
        var head = "\(request.httpMethod ?? "GET") \(target) HTTP/1.1\r\n"
        head += "Host: 127.0.0.1:7313\r\nConnection: close\r\nContent-Length: \(body.count)\r\n"
        for (name, value) in request.allHTTPHeaderFields ?? [:] {
            if ["host", "connection", "content-length", "transfer-encoding"].contains(name.lowercased()) { continue }
            guard !name.utf8.contains(where: { $0 < 0x20 || $0 == 0x7f }),
                  !value.utf8.contains(where: { $0 < 0x20 || $0 == 0x7f })
            else { throw malformed() }
            head += "\(name): \(value)\r\n"
        }
        return Data((head + "\r\n").utf8) + body
    }

    static func response(_ data: Data, eof: Bool) throws -> (Data, Int)? {
        guard data.count <= headerLimit + bodyLimit else { throw malformed() }
        guard let boundary = data.range(of: separator) else {
            if data.count > headerLimit || eof { throw malformed() }
            return nil
        }
        guard boundary.lowerBound <= headerLimit,
              let header = String(data: data[..<boundary.lowerBound], encoding: .utf8)
        else { throw malformed() }
        let lines = header.components(separatedBy: "\r\n")
        let statusLine = lines[0].split(separator: " ", maxSplits: 2)
        guard statusLine.count >= 2, ["HTTP/1.1", "HTTP/1.0"].contains(String(statusLine[0])),
              let status = Int(statusLine[1]), (200...599).contains(status)
        else { throw malformed() }
        var fields: [String: String] = [:]
        for line in lines.dropFirst() {
            guard let colon = line.firstIndex(of: ":") else { throw malformed() }
            let key = line[..<colon].lowercased()
            guard !key.isEmpty, fields[key] == nil else { throw malformed() }
            fields[key] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
        }
        let body = Data(data[boundary.upperBound...])
        if let encoding = fields["transfer-encoding"] {
            guard encoding.lowercased() == "chunked", fields["content-length"] == nil else { throw malformed() }
            if let decoded = try chunks(body) { return (decoded, status) }
        } else if let length = fields["content-length"] {
            guard !length.isEmpty, length.utf8.allSatisfy({ (48...57).contains($0) }),
                  let count = Int(length), count <= bodyLimit else { throw malformed() }
            if body.count >= count { return (Data(body.prefix(count)), status) }
        } else if eof { return (body, status) }
        if eof { throw malformed() }
        return nil
    }

    private static func chunks(_ body: Data) throws -> Data? {
        var offset = 0
        var result = Data()
        while true {
            guard let end = body.range(of: crlf, in: offset..<body.count) else { return nil }
            guard let line = String(data: body[offset..<end.lowerBound], encoding: .ascii),
                  let number = line.split(separator: ";", maxSplits: 1).first,
                  !number.isEmpty, number.allSatisfy({ $0.isHexDigit }),
                  let count = Int(number, radix: 16), count <= bodyLimit - result.count
            else { throw malformed() }
            offset = end.upperBound
            if count == 0 {
                // Empty trailer or a bounded trailer section.
                if body.count >= offset + 2 && body[offset..<offset + 2] == crlf { return result }
                if body.range(of: separator, in: offset..<body.count) != nil { return result }
                return nil
            }
            guard body.count - offset >= count + 2 else { return nil }
            result.append(body[offset..<offset + count])
            offset += count
            guard body[offset..<offset + 2] == crlf else { throw malformed() }
            offset += 2
        }
    }
}
