import AVFoundation
import Darwin
import Foundation

public struct Clip: Sendable {
    public let id: String
    public let set: String
    public let samples: [Float]
}

public func emit(_ value: [String: Any]) throws {
    let data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([10]))
}

public func milliseconds(_ start: ContinuousClock.Instant) -> Double {
    let d = start.duration(to: .now).components
    return Double(d.seconds) * 1000 + Double(d.attoseconds) / 1e15
}

public func peakRSS() -> Int64 {
    var usage = rusage()
    getrusage(RUSAGE_SELF, &usage)
    return Int64(usage.ru_maxrss) // Darwin reports bytes.
}

public func clips(_ manifest: String) throws -> [Clip] {
    let base = URL(fileURLWithPath: manifest).deletingLastPathComponent()
    return try String(contentsOfFile: manifest, encoding: .utf8).split(separator: "\n").map { row in
        let f = row.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        let audio = try AVAudioFile(forReading: base.appendingPathComponent(f[2]))
        precondition(audio.processingFormat.sampleRate == 16000 && audio.processingFormat.channelCount == 1)
        let buffer = AVAudioPCMBuffer(pcmFormat: audio.processingFormat, frameCapacity: AVAudioFrameCount(audio.length))!
        try audio.read(into: buffer)
        return Clip(id: f[0], set: f[1], samples: Array(UnsafeBufferPointer(start: buffer.floatChannelData![0], count: Int(buffer.frameLength))))
    }
}

@MainActor
public func run(_ clips: [Clip], engine: String, decode: @MainActor ([Float]) async throws -> String) async throws {
    // Every clip warms its duration-dependent kernels before measurement.
    for clip in clips { _ = try await decode(clip.samples) }
    for pass in 0..<3 {
        for clip in clips {
            let start = ContinuousClock.now
            let text = try await decode(clip.samples)
            let ms = milliseconds(start)
            try emit(["kind": "clip", "engine": engine, "id": clip.id, "set": clip.set,
                      "pass": pass, "ms": ms, "audio_s": Double(clip.samples.count) / 16000,
                      "text": text, "peak_rss_bytes": peakRSS()])
        }
    }
}
