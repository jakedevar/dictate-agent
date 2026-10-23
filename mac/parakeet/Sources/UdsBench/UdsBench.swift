import Foundation

func words(_ text: String) -> [String] {
    String(text.lowercased().map { c in c.isASCII && (c.isLetter || c.isNumber || c == "'") ? c : " " }).split(whereSeparator: { $0.isWhitespace }).map(String.init)
}
func edits(_ reference: [String], _ hypothesis: [String]) -> Int {
    var row = Array(0...hypothesis.count)
    for (i, word) in reference.enumerated() {
        var next = [i + 1]
        for (j, candidate) in hypothesis.enumerated() {
            next.append(min(row[j] + (word == candidate ? 0 : 1), row[j+1] + 1, next[j] + 1))
        }
        row = next
    }
    return row[hypothesis.count]
}
func emit(_ object: [String: Any]) throws {
    var data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    data.append(10)
    try FileHandle.standardOutput.write(contentsOf: data)
}
struct Clip {
    let id: String
    let set: String
    let wav: URL
    let reference: [String]
}
func transcribe(_ clip: Clip, socket: String, token: String) throws -> (String, Double) {
    let task = Process()
    task.executableURL = URL(fileURLWithPath: "/usr/bin/curl")
    task.arguments = ["--fail-with-body", "--silent", "--show-error", "--max-time", "180", "--unix-socket", socket, "-K", "-", "-H", "Content-Type: audio/wav", "--data-binary", "@" + clip.wav.path, "-w", "\n%{time_total}", "http://127.0.0.1:7313/v1/transcribe"]
    let input = Pipe(), output = Pipe()
    task.standardInput = input; task.standardOutput = output; task.standardError = FileHandle.standardError
    try task.run()
    // Credential bytes never enter argv, logs or the result artifact.
    try input.fileHandleForWriting.write(contentsOf: Data("header = \"Authorization: Bearer \(token)\"\n".utf8))
    try input.fileHandleForWriting.close()
    let data = output.fileHandleForReading.readDataToEndOfFile()
    task.waitUntilExit()
    guard task.terminationStatus == 0, let result = String(data: data, encoding: .utf8), let separator = result.lastIndex(of: "\n"), let seconds = Double(result[result.index(after: separator)...]), seconds > 0 else { throw CocoaError(.fileReadCorruptFile) }
    let event = try JSONSerialization.jsonObject(with: Data(result[..<separator].utf8)) as? [String: Any]
    guard let text = event?["text"] as? String, event?["type"] as? String == "transcript" else { throw CocoaError(.fileReadCorruptFile) }
    return (text, seconds * 1000)
}

@main struct UdsBench {
    static func main() throws {
        let args = CommandLine.arguments
        guard args.count == 2 else { throw CocoaError(.fileReadInvalidFileName) }
        let manifest = URL(fileURLWithPath: args[1])
        let root = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/dictated")
        let token = try String(contentsOf: root.appendingPathComponent("api-token"), encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)
        guard token.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }) else { throw CocoaError(.fileReadCorruptFile) }
        let clips = try String(contentsOf: manifest, encoding: .utf8).split(separator: "\n").map { line -> Clip in
            let fields = line.split(separator: "\t").map(String.init)
            guard fields.count == 4 else { throw CocoaError(.fileReadCorruptFile) }
            let base = manifest.deletingLastPathComponent()
            return Clip(id: fields[0], set: fields[1], wav: base.appendingPathComponent(fields[2]), reference: words(try String(contentsOf: base.appendingPathComponent(fields[3]), encoding: .utf8)))
        }
        guard clips.count == 33, Set(clips.map(\.id)).count == clips.count else { throw CocoaError(.fileReadCorruptFile) }
        let socket = root.appendingPathComponent("run/api.sock").path
        for clip in clips { _ = try transcribe(clip, socket: socket, token: token) }
        var times: [String: [Double]] = [:]
        var errors: [String: [Int]] = [:], counts: [String: [Int]] = [:]
        for pass in 0..<3 {
            for clip in clips {
                let (text, ms) = try transcribe(clip, socket: socket, token: token)
                let distance = edits(clip.reference, words(text))
                times[clip.set, default: []].append(ms)
                errors[clip.set, default: [0, 0, 0]][pass] += distance
                counts[clip.set, default: [0, 0, 0]][pass] += clip.reference.count
                try emit(["kind": "clip", "id": clip.id, "set": clip.set, "pass": pass, "text": text, "ms": ms, "edits": distance, "words": clip.reference.count])
            }
        }
        for set in times.keys.sorted() {
            let values = times[set]!.sorted()
            try emit(["kind": "summary", "set": set, "measurements": values.count, "p50_ms": values[Int(ceil(Double(values.count) * 0.5)) - 1], "p95_ms": values[Int(ceil(Double(values.count) * 0.95)) - 1], "errors": errors[set]!, "words": counts[set]!, "wer_percent": zip(errors[set]!, counts[set]!).map { Double($0) / Double($1) * 100 }])
        }
    }
}
