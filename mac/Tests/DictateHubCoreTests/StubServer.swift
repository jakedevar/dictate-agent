import Foundation
import Network

/// A tiny HTTP/1.1 server on an ephemeral 127.0.0.1 port. It reads one
/// request per connection (headers + Content-Length body), records it,
/// answers with a canned response, and closes.
final class StubServer: @unchecked Sendable {
    struct Recorded: Sendable {
        let method: String
        let target: String
        let headers: [String: String]  // lower-cased names
        let body: Data
    }

    struct Response: Sendable {
        var status: Int
        var reason: String
        var body: Data
        var contentType = "application/json"
        /// Wait this long before answering (timeout tests).
        var delay: TimeInterval = 0
    }

    private let listener: NWListener
    private let queue = DispatchQueue(label: "stub-server")
    private let lock = NSLock()
    private var _requests: [Recorded] = []
    private let respond: @Sendable (Recorded) -> Response

    private(set) var port: UInt16 = 0
    var endpoint: String { "http://127.0.0.1:\(port)" }
    var requests: [Recorded] { lock.withLock { _requests } }

    init(respond: @escaping @Sendable (Recorded) -> Response) throws {
        let params = NWParameters.tcp
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: "127.0.0.1", port: .any)
        params.allowLocalEndpointReuse = true
        listener = try NWListener(using: params)
        self.respond = respond
    }

    func start() async throws {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            let once = OnceFlag()
            listener.stateUpdateHandler = { [weak self] state in
                switch state {
                case .ready:
                    if once.take() {
                        self?.port = self?.listener.port?.rawValue ?? 0
                        cont.resume()
                    }
                case .failed(let e):
                    if once.take() { cont.resume(throwing: e) }
                default: break
                }
            }
            listener.newConnectionHandler = { [weak self] conn in self?.accept(conn) }
            listener.start(queue: queue)
        }
    }

    func stop() { listener.cancel() }

    private func accept(_ conn: NWConnection) {
        conn.start(queue: queue)
        read(conn, buffer: Data())
    }

    private func read(_ conn: NWConnection, buffer: Data) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 1 << 20) { [weak self] data, _, done, error in
            guard let self else { return }
            var buf = buffer
            if let data { buf.append(data) }
            if let req = Self.parse(buf) {
                self.lock.withLock { self._requests.append(req) }
                let resp = self.respond(req)
                let send: @Sendable () -> Void = { self.write(conn, resp) }
                if resp.delay > 0 {
                    self.queue.asyncAfter(deadline: .now() + resp.delay, execute: send)
                } else {
                    send()
                }
            } else if done || error != nil {
                conn.cancel()
            } else {
                self.read(conn, buffer: buf)
            }
        }
    }

    private func write(_ conn: NWConnection, _ resp: Response) {
        var head = "HTTP/1.1 \(resp.status) \(resp.reason)\r\n"
        head += "Content-Type: \(resp.contentType)\r\n"
        head += "Content-Length: \(resp.body.count)\r\n"
        head += "Connection: close\r\n\r\n"
        var out = Data(head.utf8)
        out.append(resp.body)
        conn.send(content: out, completion: .contentProcessed { _ in conn.cancel() })
    }

    /// A complete request, or nil if more bytes are needed.
    static func parse(_ buf: Data) -> Recorded? {
        guard let sep = buf.range(of: Data("\r\n\r\n".utf8)) else { return nil }
        let head = String(decoding: buf[buf.startIndex ..< sep.lowerBound], as: UTF8.self)
        var lines = head.components(separatedBy: "\r\n")
        let requestLine = lines.removeFirst().split(separator: " ")
        guard requestLine.count >= 2 else { return nil }
        var headers: [String: String] = [:]
        for line in lines {
            guard let colon = line.firstIndex(of: ":") else { continue }
            headers[line[..<colon].lowercased()] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
        }
        let length = Int(headers["content-length"] ?? "0") ?? 0
        let body = buf[sep.upperBound...]
        guard body.count >= length else { return nil }
        return Recorded(method: String(requestLine[0]), target: String(requestLine[1]),
                        headers: headers, body: Data(body.prefix(length)))
    }
}

private final class OnceFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var taken = false
    func take() -> Bool {
        lock.withLock {
            defer { taken = true }
            return !taken
        }
    }
}
