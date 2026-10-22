import AVFoundation
import DictateHubCore

/// Collects converted 16 kHz mono Float32 samples from the audio thread.
/// `@unchecked Sendable`: every mutable field is guarded by `lock`, and the
/// converter is only touched from the tap callback (one thread at a time).
final class SampleSink: @unchecked Sendable {
    private let lock = NSLock()
    private var samples: [Float] = []
    private var limitFired = false
    private let converter: AVAudioConverter
    private let inputRate: Double
    let outputFormat: AVAudioFormat
    let maxSamples: Int
    let onLimit: @Sendable () -> Void

    init(input: AVAudioFormat, maxSeconds: Double, onLimit: @escaping @Sendable () -> Void) throws {
        guard let out = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Double(WAV.sampleRate),
                                      channels: 1, interleaved: false),
              let conv = AVAudioConverter(from: input, to: out)
        else { throw RecorderError.unsupportedFormat }
        conv.downmix = true
        converter = conv
        outputFormat = out
        inputRate = input.sampleRate
        maxSamples = Int(maxSeconds * Double(WAV.sampleRate))
        self.onLimit = onLimit
        samples.reserveCapacity(WAV.sampleRate * 10)
    }

    func append(_ buffer: AVAudioPCMBuffer) {
        let capacity = AVAudioFrameCount((Double(buffer.frameLength) * Double(WAV.sampleRate) / inputRate).rounded(.up)) + 64
        guard let out = AVAudioPCMBuffer(pcmFormat: outputFormat, frameCapacity: capacity) else { return }
        var fed = false
        var error: NSError?
        let status = converter.convert(to: out, error: &error) { _, inputStatus in
            if fed {
                inputStatus.pointee = .noDataNow
                return nil
            }
            fed = true
            inputStatus.pointee = .haveData
            return buffer
        }
        guard status != .error, let channel = out.floatChannelData?[0] else { return }
        let chunk = UnsafeBufferPointer(start: channel, count: Int(out.frameLength))

        var fire = false
        lock.lock()
        let room = maxSamples - samples.count
        if room > 0 { samples.append(contentsOf: chunk.prefix(room)) }
        if samples.count >= maxSamples && !limitFired {
            limitFired = true
            fire = true
        }
        lock.unlock()
        if fire { onLimit() }
    }

    func drain() -> [Float] {
        lock.lock()
        defer { lock.unlock() }
        let s = samples
        samples = []
        return s
    }
}

enum RecorderError: Error {
    case noInputDevice
    case unsupportedFormat

    var message: String {
        switch self {
        case .noInputDevice: return "No microphone input device"
        case .unsupportedFormat: return "Microphone format not supported"
        }
    }
}

/// Microphone capture for one clip. A fresh `AVAudioEngine` per clip, so a
/// device change between dictations is picked up without observers.
@MainActor
final class AudioRecorder {
    static let maxSeconds: Double = 120

    private var engine: AVAudioEngine?
    private var sink: SampleSink?

    var isRecording: Bool { engine != nil }

    /// Start capturing. `onLimit` runs on the main actor when the 120 s cap
    /// is reached; capture keeps running until `stop()`.
    func start(onLimit: @escaping @MainActor () -> Void) throws {
        guard engine == nil else { return }
        let engine = AVAudioEngine()
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw RecorderError.noInputDevice }
        let sink = try SampleSink(input: format, maxSeconds: Self.maxSeconds) {
            Task { @MainActor in onLimit() }
        }
        input.installTap(onBus: 0, bufferSize: 4096, format: format) { buffer, _ in
            sink.append(buffer)
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw error
        }
        self.engine = engine
        self.sink = sink
    }

    /// Stop capturing and return the 16 kHz mono samples.
    func stop() -> [Float] {
        guard let engine, let sink else { return [] }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        self.engine = nil
        self.sink = nil
        return sink.drain()
    }
}
