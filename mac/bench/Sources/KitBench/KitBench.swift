import BenchSupport
import Foundation
import WhisperKit

@main struct KitBench {
    static func main() async throws {
        let args = CommandLine.arguments
        let input = try clips(args[1])
        let start = ContinuousClock.now
        let pipe = try await WhisperKit(WhisperKitConfig(
            model: "openai_whisper-large-v3-v20240930_turbo",
            downloadBase: URL(fileURLWithPath: args[2]),
            tokenizerFolder: URL(fileURLWithPath: args[2]).appendingPathComponent("tokenizers"),
            verbose: false, prewarm: true, load: true))
        try emit(["kind": "load", "engine": "whisperkit", "ms": milliseconds(start), "peak_rss_bytes": peakRSS()])
        if args.count > 3 { return } // fetch/precompile separately from cold-process measurement
        let options = DecodingOptions(language: "en", temperature: 0, temperatureFallbackCount: 0,
                                      skipSpecialTokens: true, withoutTimestamps: true, concurrentWorkerCount: 1)
        try await run(input, engine: "whisperkit") { samples in
            try await pipe.transcribe(audioArray: samples, decodeOptions: options).map(\.text).joined(separator: " ")
        }
    }
}
