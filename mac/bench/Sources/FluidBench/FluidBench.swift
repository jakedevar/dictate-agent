import BenchSupport
import FluidAudio
import Foundation

@main struct FluidBench {
    static func main() async throws {
        let args = CommandLine.arguments
        let input = try clips(args[1])
        let start = ContinuousClock.now
        // FluidAudio resolves its repository beside the supplied directory.
        // Supply the repository directory itself to keep every artifact in models/.
        let directory = URL(fileURLWithPath: args[2]).appendingPathComponent("parakeet-tdt-0.6b-v2")
        let models = try await AsrModels.downloadAndLoad(to: directory, version: .v2)
        let manager = AsrManager(config: .default)
        try await manager.loadModels(models)
        try emit(["kind": "load", "engine": "parakeet-v2", "ms": milliseconds(start), "peak_rss_bytes": peakRSS()])
        if args.count > 3 { return }
        try await run(input, engine: "parakeet-v2") { samples in
            var state = try TdtDecoderState()
            return try await manager.transcribe(samples, decoderState: &state).text
        }
    }
}
