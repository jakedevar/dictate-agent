import CryptoKit
import Darwin
import FluidAudio
import Foundation
import ParakeetProtocol

// Embed the pinned benchmark hashes in the binary: no mutable sidecar can
// authorize different model bytes.
func verifyModels(_ root: URL) throws {
    let manifest = modelManifest
    for row in manifest.split(separator: "\n") {
        let fields = row.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: true)
        guard fields.count == 2 else { throw FrameError.invalidHeader }
        let path = String(fields[1]).trimmingCharacters(in: .whitespaces)
        let url = root.appendingPathComponent(path)
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        guard attributes[.type] as? FileAttributeType == .typeRegular else { throw FrameError.invalidHeader }
        let input = try FileHandle(forReadingFrom: url)
        defer { try? input.close() }
        var digest = SHA256()
        while let bytes = try input.read(upToCount: 1024 * 1024), !bytes.isEmpty { digest.update(data: bytes) }
        let hex = digest.finalize().map { String(format: "%02x", $0) }.joined()
        guard hex == fields[0] else { throw FrameError.invalidHeader }
    }
}

@main struct DictateParakeet {
    static func main() async {
        // FluidAudio's debug/info diagnostics contain transcripts. Drop them
        // from console AND unified logging before constructing any ASR object.
        AppLogger.minimumLevel = .fault
        AppLogger.mirrorsToConsole = false
        let output = FileHandle(fileDescriptor: dup(STDOUT_FILENO), closeOnDealloc: true)
        dup2(STDERR_FILENO, STDOUT_FILENO) // library print() cannot corrupt the protocol
        do {
            let args = CommandLine.arguments
            guard args.count == 3, args[1] == "--models" || args[1] == "--prepare" || args[1] == "--verify-models" else { throw FrameError.invalidHeader }
            let directory = URL(fileURLWithPath: args[2], isDirectory: true)
            if args[1] == "--prepare" { _ = try await AsrModels.download(to: directory, version: .v2) }
            try verifyModels(directory)
            if args[1] == "--verify-models" { return }
            let models = try AsrModels.loadLocal(from: directory, version: .v2)
            let manager = AsrManager(config: .default)
            try await manager.loadModels(models)
            if args[1] == "--prepare" { return }
            try output.write(contentsOf: Data("{\"ready\":true}\n".utf8))
            let input = FileHandle.standardInput
            while let prefix = try readExactly(input, count: 4, allowEOF: true) {
                let size = try Frame.size(prefix)
                let frame = try Frame(body: readExactly(input, count: size)!)
                let started = ContinuousClock.now
                do {
                    var state = try TdtDecoderState()
                    let text = frame.samples.isEmpty ? "" : try await manager.transcribe(frame.samples, decoderState: &state).text
                    let elapsed = started.duration(to: .now).components
                    let ms = Double(elapsed.seconds) * 1000 + Double(elapsed.attoseconds) / 1e15
                    try emit(["id": frame.id, "text": text, "ms": ms], output)
                } catch {
                    try emit(["id": frame.id, "error": "decode_failed"], output)
                }
            }
        } catch {
            // Error descriptions from third parties can contain input text.
            try? FileHandle.standardError.write(contentsOf: Data("dictate-parakeet: startup or protocol failure\n".utf8))
            exit(1)
        }
    }
    static func emit(_ value: [String: Any], _ output: FileHandle) throws {
        var bytes = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        guard bytes.count < 64 * 1024 else { throw FrameError.invalidSize }
        bytes.append(10)
        try output.write(contentsOf: bytes)
    }
}
