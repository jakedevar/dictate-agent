import Foundation

/// The clip format the hub expects: 16 kHz, mono, signed 16-bit little-endian
/// PCM in a RIFF/WAVE container (`Content-Type: audio/wav`).
public enum WAV {
    public static let sampleRate: Int = 16_000
    public static let channels: Int = 1
    public static let bitsPerSample: Int = 16
    /// Bytes in the canonical 44-byte PCM header.
    public static let headerSize: Int = 44

    /// Convert one Float32 sample in [-1, 1] to Int16. Out-of-range input is
    /// clamped (a hot microphone can overshoot), NaN becomes silence, and the
    /// scale is symmetric (×32767) so +1.0 and -1.0 map to ±32767.
    @inlinable
    public static func int16(from sample: Float) -> Int16 {
        guard !sample.isNaN else { return 0 }
        let clamped = min(max(sample, -1.0), 1.0)
        return Int16((clamped * 32_767.0).rounded())
    }

    /// Convert Float32 samples to Int16 PCM.
    public static func int16Samples(from samples: [Float]) -> [Int16] {
        samples.map(int16(from:))
    }

    /// Encode Int16 mono PCM at `sampleRate` as an in-memory WAV file.
    public static func encode(samples: [Int16], sampleRate: Int = WAV.sampleRate) -> Data {
        let dataBytes = samples.count * 2
        let byteRate = sampleRate * channels * bitsPerSample / 8
        let blockAlign = channels * bitsPerSample / 8

        var out = Data(capacity: headerSize + dataBytes)
        out.append(contentsOf: Array("RIFF".utf8))
        out.appendLE(UInt32(36 + dataBytes))
        out.append(contentsOf: Array("WAVE".utf8))
        out.append(contentsOf: Array("fmt ".utf8))
        out.appendLE(UInt32(16))  // PCM fmt chunk size
        out.appendLE(UInt16(1))  // format tag: PCM
        out.appendLE(UInt16(channels))
        out.appendLE(UInt32(sampleRate))
        out.appendLE(UInt32(byteRate))
        out.appendLE(UInt16(blockAlign))
        out.appendLE(UInt16(bitsPerSample))
        out.append(contentsOf: Array("data".utf8))
        out.appendLE(UInt32(dataBytes))
        samples.withUnsafeBufferPointer { buf in
            for s in buf { out.appendLE(UInt16(bitPattern: s)) }
        }
        return out
    }

    /// Encode Float32 mono samples at `sampleRate` as a 16-bit WAV file.
    public static func encode(floatSamples: [Float], sampleRate: Int = WAV.sampleRate) -> Data {
        encode(samples: int16Samples(from: floatSamples), sampleRate: sampleRate)
    }

    /// Duration in seconds of `sampleCount` samples at the hub rate.
    public static func duration(sampleCount: Int) -> Double {
        Double(sampleCount) / Double(sampleRate)
    }
}

extension Data {
    @usableFromInline
    mutating func appendLE<T: FixedWidthInteger>(_ value: T) {
        var le = value.littleEndian
        Swift.withUnsafeBytes(of: &le) { append(contentsOf: $0) }
    }
}
