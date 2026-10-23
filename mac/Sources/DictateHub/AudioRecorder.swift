import AVFoundation
import DictateHubCore

/// Converts tap buffers to 16 kHz mono Float32 and feeds a `ClipAccumulator`.
/// `@unchecked Sendable`: every mutable field is guarded by `lock`, and the
/// converter is only touched from the tap callback (one thread at a time).
final class SampleSink: @unchecked Sendable {
    private let lock = NSLock()
    private var acc: ClipAccumulator
    private var awaitingFirstAudio = false
    private let converter: AVAudioConverter
    private let inputRate: Double
    let outputFormat: AVAudioFormat
    /// Fires (on the audio thread) when the clip reaches the cap.
    private var onLimit: (@Sendable () -> Void)?
    /// Fires (on the audio thread) with the first audio of a clip.
    private var onFirstAudio: (@Sendable () -> Void)?

    init(input: AVAudioFormat, preRollSamples: Int, maxSeconds: Double) throws {
        guard let out = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Double(WAV.sampleRate),
                                      channels: 1, interleaved: false),
              let conv = AVAudioConverter(from: input, to: out)
        else { throw RecorderError.unsupportedFormat }
        conv.downmix = true
        converter = conv
        outputFormat = out
        inputRate = input.sampleRate
        acc = ClipAccumulator(preRollSamples: preRollSamples, maxSamples: Int(maxSeconds * Double(WAV.sampleRate)))
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
        guard status != .error, let channel = out.floatChannelData?[0], out.frameLength > 0 else { return }
        let chunk = UnsafeBufferPointer(start: channel, count: Int(out.frameLength))

        lock.lock()
        let first = acc.isRecording && awaitingFirstAudio
        if first { awaitingFirstAudio = false }
        let limit = acc.append(chunk)
        let onLimit = self.onLimit, onFirst = self.onFirstAudio
        lock.unlock()
        if first { onFirst?() }
        if limit { onLimit?() }
    }

    /// Start a clip. Returns the pre-roll samples it starts with; with none,
    /// `onFirstAudio` fires when the first converted buffer arrives.
    func beginClip(onFirstAudio: @escaping @Sendable () -> Void, onLimit: @escaping @Sendable () -> Void) -> Int {
        lock.lock()
        defer { lock.unlock() }
        self.onFirstAudio = onFirstAudio
        self.onLimit = onLimit
        let seeded = acc.beginClip()
        awaitingFirstAudio = seeded == 0
        return seeded
    }

    func endClip() -> [Float] {
        lock.lock()
        defer { lock.unlock() }
        awaitingFirstAudio = false
        onFirstAudio = nil
        onLimit = nil
        return acc.endClip()
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

/// Microphone capture.
///
/// Cold (default): a fresh `AVAudioEngine` per clip, started at key-down and
/// torn down at release, so the microphone is open only while dictating and a
/// device change between clips needs no observers. `onReady` fires when the
/// first audio arrives (the start cue plays then, so it means "speak now"),
/// with the measured key-down → first-audio latency.
///
/// Warm (`keepWarm`, opt-in): one engine runs while idle and keeps the last
/// `preRollSeconds` in a ring; a clip starts with that pre-roll, so a word
/// spoken right at key-down is kept. Never started unless microphone access
/// is already granted, so it can never raise the permission prompt. A device
/// or format change rebuilds the engine.
@MainActor
final class AudioRecorder {
    static let maxSeconds: Double = 120
    static let preRollSeconds: Double = 0.3

    private var engine: AVAudioEngine?
    private var sink: SampleSink?
    private var configObserver: NSObjectProtocol?
    private(set) var isRecording = false
    /// The running engine was started for idle pre-roll (not for one clip).
    private var engineWarm = false

    /// Keep the engine running while idle (with pre-roll).
    var keepWarm = false {
        didSet { if keepWarm != oldValue { reconcileWarm() } }
    }

    var isWarm: Bool { engine != nil && engineWarm }

    static var micAuthorized: Bool {
        AVCaptureDevice.authorizationStatus(for: .audio) == .authorized
    }

    /// Start a clip. `onReady(latencyMs, preRollMs)` runs on the main actor
    /// once audio is flowing; `onLimit` when the 120 s cap is reached (capture
    /// keeps running until `stop()`).
    func start(keyDown: TimeInterval? = nil,
               onReady: @escaping @MainActor (Int, Int) -> Void,
               onLimit: @escaping @MainActor () -> Void) throws {
        guard !isRecording else { return }
        if engine == nil { try startEngine(preRollSamples: 0) }
        guard let sink else { throw RecorderError.noInputDevice }
        let t0 = keyDown ?? ProcessInfo.processInfo.systemUptime
        let seeded = sink.beginClip(
            onFirstAudio: {
                let ms = Int((ProcessInfo.processInfo.systemUptime - t0) * 1000)
                Task { @MainActor in onReady(ms, 0) }
            },
            onLimit: { Task { @MainActor in onLimit() } })
        isRecording = true
        if seeded > 0 {
            onReady(0, seeded * 1000 / WAV.sampleRate)
        }
    }

    /// Stop the clip and return its 16 kHz mono samples. A warm engine keeps
    /// running; a cold one is torn down.
    func stop() -> [Float] {
        guard isRecording else { return [] }
        isRecording = false
        let samples = sink?.endClip() ?? []
        if !(keepWarm && engineWarm && Self.micAuthorized) {
            stopEngine()
        }
        reconcileWarm()
        return samples
    }

    /// Re-check whether the idle engine should run (call when mic access or
    /// the setting may have changed).
    func reconcileWarm() {
        guard !isRecording else { return }
        let want = keepWarm && Self.micAuthorized
        if want && engine == nil {
            do {
                try startEngine(preRollSamples: Int(Self.preRollSeconds * Double(WAV.sampleRate)))
                Log.info("microphone kept warm (\(Int(Self.preRollSeconds * 1000)) ms pre-roll)")
            } catch {
                Log.error("warm microphone failed to start: \(error)")
            }
        } else if !want && engine != nil {
            stopEngine()
            Log.info("warm microphone closed")
        }
    }

    func shutdown() {
        isRecording = false
        stopEngine()
    }

    private func startEngine(preRollSamples: Int) throws {
        let engine = AVAudioEngine()
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw RecorderError.noInputDevice }
        let sink = try SampleSink(input: format, preRollSamples: preRollSamples, maxSeconds: Self.maxSeconds)
        // `@Sendable` keeps this closure off the main actor: AVFAudio calls it on
        // its realtime queue, and a main-actor-isolated closure there trips Swift
        // 6's runtime isolation check (EXC_BREAKPOINT on the first buffer, #3824).
        input.installTap(onBus: 0, bufferSize: 2048, format: format) { @Sendable buffer, _ in
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
        engineWarm = preRollSamples > 0
        // A device or format change stops the engine. Rebuild a warm one; a
        // cold clip in flight keeps what it captured until release.
        configObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.configurationChanged() }
        }
    }

    private func configurationChanged() {
        Log.info("audio configuration changed")
        guard !isRecording else { return }
        stopEngine()
        reconcileWarm()
    }

    private func stopEngine() {
        if let configObserver { NotificationCenter.default.removeObserver(configObserver) }
        configObserver = nil
        if let engine {
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
        }
        engine = nil
        sink = nil
        engineWarm = false
    }
}
