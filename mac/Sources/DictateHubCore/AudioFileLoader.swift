import AVFoundation
import Foundation

public enum AudioFileError: Error, Equatable, Sendable {
    case unreadable(String)
    case unsupportedFormat
    case conversionFailed
    case empty

    public var message: String {
        switch self {
        case .unreadable(let why): return "Cannot read the audio file: \(why)"
        case .unsupportedFormat: return "Audio format not supported"
        case .conversionFailed: return "Audio conversion failed"
        case .empty: return "The audio file has no samples"
        }
    }
}

/// Reads an audio file (WAV or anything Core Audio opens) and converts it to
/// the clip format the microphone path produces: 16 kHz mono Float32. File
/// mode (`--transcribe-file`) feeds these samples into the same WAV encoder,
/// hub client and paste path as a live dictation.
public enum AudioFileLoader {
    public static func load(_ url: URL) throws -> [Float] {
        let file: AVAudioFile
        do {
            file = try AVAudioFile(forReading: url)
        } catch {
            throw AudioFileError.unreadable(error.localizedDescription)
        }
        let input = file.processingFormat
        guard input.sampleRate > 0, input.channelCount > 0, file.length > 0 else {
            throw file.length == 0 ? AudioFileError.empty : AudioFileError.unsupportedFormat
        }
        guard let output = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Double(WAV.sampleRate),
                                         channels: 1, interleaved: false),
              let converter = AVAudioConverter(from: input, to: output),
              let source = AVAudioPCMBuffer(pcmFormat: input, frameCapacity: AVAudioFrameCount(file.length))
        else { throw AudioFileError.unsupportedFormat }
        converter.downmix = true
        do {
            try file.read(into: source)
        } catch {
            throw AudioFileError.unreadable(error.localizedDescription)
        }

        let ratio = Double(WAV.sampleRate) / input.sampleRate
        let capacity = AVAudioFrameCount((Double(source.frameLength) * ratio).rounded(.up)) + 1024
        guard let out = AVAudioPCMBuffer(pcmFormat: output, frameCapacity: capacity) else {
            throw AudioFileError.conversionFailed
        }
        var fed = false
        var error: NSError?
        let status = converter.convert(to: out, error: &error) { _, inputStatus in
            if fed {
                inputStatus.pointee = .endOfStream
                return nil
            }
            fed = true
            inputStatus.pointee = .haveData
            return source
        }
        guard status != .error, let channel = out.floatChannelData?[0] else {
            throw AudioFileError.conversionFailed
        }
        let samples = Array(UnsafeBufferPointer(start: channel, count: Int(out.frameLength)))
        guard !samples.isEmpty else { throw AudioFileError.empty }
        return samples
    }
}
