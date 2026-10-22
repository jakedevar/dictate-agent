// One-shot stub hub for file-probe.sh (replaces the former Python stub).
// usage: file-probe-stub PORT OUT_PREFIX
// Accepts a single connection on 127.0.0.1:PORT, writes the request line plus
// headers to OUT_PREFIX.head and the body to OUT_PREFIX.body, answers 200 with
// a fixed transcript JSON and exits. Never contacts anything.
import Foundation

setvbuf(stdout, nil, _IOLBF, 0)
let args = CommandLine.arguments
guard args.count == 3, let port = UInt16(args[1]) else {
    FileHandle.standardError.write(Data("usage: file-probe-stub PORT OUT_PREFIX\n".utf8))
    exit(2)
}
let outPrefix = args[2]

let listener = socket(AF_INET, SOCK_STREAM, 0)
var yes: Int32 = 1
setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
var addr = sockaddr_in()
addr.sin_family = sa_family_t(AF_INET)
addr.sin_port = port.bigEndian
addr.sin_addr.s_addr = inet_addr("127.0.0.1")
let bound = withUnsafePointer(to: &addr) {
    $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
        bind(listener, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
    }
}
guard bound == 0, listen(listener, 1) == 0 else {
    perror("bind/listen")
    exit(1)
}
let conn = accept(listener, nil, nil)
guard conn >= 0 else { perror("accept"); exit(1) }

var data = Data()
var buf = [UInt8](repeating: 0, count: 65536)
let separator = Data("\r\n\r\n".utf8)
var headerEnd: Range<Data.Index>?
while headerEnd == nil {
    let n = read(conn, &buf, buf.count)
    if n <= 0 { break }
    data.append(buf, count: n)
    headerEnd = data.range(of: separator)
}
guard let end = headerEnd else {
    FileHandle.standardError.write(Data("no complete request header\n".utf8))
    exit(1)
}
let head = String(decoding: data[..<end.lowerBound], as: UTF8.self)
var body = Data(data[end.upperBound...])
var length = 0
for line in head.components(separatedBy: "\r\n").dropFirst() {
    let parts = line.split(separator: ":", maxSplits: 1).map { $0.trimmingCharacters(in: .whitespaces) }
    if parts.count == 2, parts[0].lowercased() == "content-length" { length = Int(parts[1]) ?? 0 }
}
while body.count < length {
    let n = read(conn, &buf, buf.count)
    if n <= 0 { break }
    body.append(buf, count: n)
}
try body.write(to: URL(fileURLWithPath: outPrefix + ".body"))
try (head.replacingOccurrences(of: "\r\n", with: "\n") + "\n").write(
    toFile: outPrefix + ".head", atomically: false, encoding: .utf8)

let reply = Data(
    #"{"type":"transcript","text":"stub transcript for file mode","route":"type","word_count":5,"model":"stub"}"#.utf8)
let header = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \(reply.count)\r\nConnection: close\r\n\r\n"
var out = Data(header.utf8)
out.append(reply)
out.withUnsafeBytes { raw in
    var sent = 0
    while sent < raw.count {
        let n = write(conn, raw.baseAddress! + sent, raw.count - sent)
        if n <= 0 { break }
        sent += n
    }
}
close(conn)
close(listener)
