import Foundation
import AVFoundation
import ParakeetProtocol

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

// Diagnostic mode isolates the installed helper from server VAD and text rules,
// keeping exactly the same model and binary used by the live UDS run.
final class HelperClient {
    let task = Process()
    let input = Pipe(), output = Pipe()
    var id: UInt64 = 0
    init(root: URL) throws {
        task.executableURL = root.appendingPathComponent("bin/dictate-parakeet")
        task.arguments = ["--models", root.appendingPathComponent("models/parakeet-tdt-0.6b-v2").path]
        task.standardInput = input; task.standardOutput = output; task.standardError = FileHandle.standardError
        try task.run()
        guard try line() == Data("{\"ready\":true}\n".utf8) else { throw CocoaError(.fileReadCorruptFile) }
    }
    deinit {
        try? input.fileHandleForWriting.close()
        if task.isRunning { task.terminate() }
        task.waitUntilExit()
    }
    func line() throws -> Data {
        var bytes = Data()
        while bytes.last != 10 {
            // Darwin FileHandle can fill the requested count before returning;
            // a line reader must not wait for bytes from the next request.
            guard let part = try readExactly(output.fileHandleForReading, count: 1), !part.isEmpty else { throw CocoaError(.fileReadCorruptFile) }
            bytes.append(part)
            guard bytes.count <= 64 * 1024, bytes.last == 10 || bytes.count < 64 * 1024 else { throw CocoaError(.fileReadCorruptFile) }
        }
        return bytes
    }
    func transcribe(_ clip: Clip) throws -> (String, Double) {
        let audio = try AVAudioFile(forReading: clip.wav)
        guard audio.processingFormat.sampleRate == 16000, audio.processingFormat.channelCount == 1, audio.length <= Frame.maxSamples else { throw CocoaError(.fileReadCorruptFile) }
        let buffer = AVAudioPCMBuffer(pcmFormat: audio.processingFormat, frameCapacity: AVAudioFrameCount(audio.length))!
        try audio.read(into: buffer)
        let samples = UnsafeBufferPointer(start: buffer.floatChannelData![0], count: Int(buffer.frameLength))
        id += 1
        let header = Data("{\"id\":\(id),\"sample_rate\":16000}".utf8)
        func u32(_ n: Int) -> Data { withUnsafeBytes(of: UInt32(n).littleEndian) { Data($0) } }
        // ARM Mac floats are little-endian, matching the production pipe.
        let pcm = samples.withMemoryRebound(to: UInt8.self) { Data($0) }
        let frame = u32(4 + header.count + pcm.count) + u32(header.count) + header + pcm
        let started = ContinuousClock.now
        try input.fileHandleForWriting.write(contentsOf: frame)
        let event = try JSONSerialization.jsonObject(with: line()) as? [String: Any]
        let elapsed = started.duration(to: .now).components
        guard (event?["id"] as? UInt64) == id, let text = event?["text"] as? String else { throw CocoaError(.fileReadCorruptFile) }
        return (text, Double(elapsed.seconds) * 1000 + Double(elapsed.attoseconds) / 1e15)
    }
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
    static func main() async throws {
        let args = CommandLine.arguments
        guard args.count == 2 || (args.count == 3 && args[2] == "--helper") else { throw CocoaError(.fileReadInvalidFileName) }
        let helperMode = args.count == 3
        let manifest = URL(fileURLWithPath: args[1])
        let root = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/dictated")
        let token = helperMode ? "" : try String(contentsOf: root.appendingPathComponent("api-token"), encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)
        guard token.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }) else { throw CocoaError(.fileReadCorruptFile) }
        let clips = try String(contentsOf: manifest, encoding: .utf8).split(separator: "\n").map { line -> Clip in
            let fields = line.split(separator: "\t").map(String.init)
            guard fields.count == 4 else { throw CocoaError(.fileReadCorruptFile) }
            let base = manifest.deletingLastPathComponent()
            return Clip(id: fields[0], set: fields[1], wav: base.appendingPathComponent(fields[2]), reference: words(try String(contentsOf: base.appendingPathComponent(fields[3]), encoding: .utf8)))
        }
        guard clips.count == 33, Set(clips.map(\.id)).count == clips.count else { throw CocoaError(.fileReadCorruptFile) }
        let socket = root.appendingPathComponent("run/api.sock").path
        let helper = helperMode ? try HelperClient(root: root) : nil
        func decode(_ clip: Clip) throws -> (String, Double) {
            if let helper { return try helper.transcribe(clip) }
            return try transcribe(clip, socket: socket, token: token)
        }
        for clip in clips {
            _ = try decode(clip)
            // Respect the unchanged S33 120 requests/minute limit. This pause
            // is outside curl's measured HTTP time.
            if !helperMode { try await Task.sleep(for: .milliseconds(510)) }
        }
        var times: [String: [Double]] = [:]
        var errors: [String: [Int]] = [:], counts: [String: [Int]] = [:]
        for pass in 0..<3 {
            for clip in clips {
                let (text, ms) = try decode(clip)
                let distance = edits(clip.reference, words(text))
                times[clip.set, default: []].append(ms)
                errors[clip.set, default: [0, 0, 0]][pass] += distance
                counts[clip.set, default: [0, 0, 0]][pass] += clip.reference.count
                try emit(["kind": "clip", "id": clip.id, "set": clip.set, "pass": pass, "text": text, "ms": ms, "edits": distance, "words": clip.reference.count])
                if !helperMode { try await Task.sleep(for: .milliseconds(510)) }
            }
        }
        for set in times.keys.sorted() {
            let values = times[set]!.sorted()
            try emit(["kind": "summary", "set": set, "measurements": values.count, "p50_ms": values[Int(ceil(Double(values.count) * 0.5)) - 1], "p95_ms": values[Int(ceil(Double(values.count) * 0.95)) - 1], "errors": errors[set]!, "words": counts[set]!, "wer_percent": zip(errors[set]!, counts[set]!).map { Double($0) / Double($1) * 100 }])
        }
    }
}
