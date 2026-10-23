import Foundation

public enum FrameError: Error { case invalidSize, truncated, invalidHeader, invalidSamples }
public struct Request: Decodable {
    public let id: UInt64
    public let sample_rate: Int
}
public struct Frame {
    public static let maxSamples = 600 * 16_000
    public static let maxHeader = 4096
    public static let maxBytes = 4 + maxHeader + maxSamples * 4
    public let id: UInt64
    public let samples: [Float]

    // u32 little-endian frame bytes (excluding this prefix), u32 little-endian
    // JSON header bytes, UTF-8 {id,sample_rate}, then mono f32le PCM.
    public static func size(_ prefix: Data) throws -> Int {
        guard prefix.count == 4 else { throw FrameError.truncated }
        let value = Int(prefix.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self).littleEndian })
        guard value >= 4, value <= maxBytes else { throw FrameError.invalidSize }
        return value
    }
    public init(body: Data) throws {
        guard body.count >= 4, body.count <= Self.maxBytes else { throw FrameError.invalidSize }
        let headerSize = Int(body.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self).littleEndian })
        guard headerSize > 0, headerSize <= Self.maxHeader, headerSize <= body.count - 4 else { throw FrameError.invalidHeader }
        let header = try JSONDecoder().decode(Request.self, from: body.subdata(in: 4..<4+headerSize))
        guard header.sample_rate == 16000 else { throw FrameError.invalidHeader }
        let count = body.count - 4 - headerSize
        guard count % 4 == 0, count / 4 <= Self.maxSamples else { throw FrameError.invalidSamples }
        let samples: [Float] = body.withUnsafeBytes { bytes in
            stride(from: 4 + headerSize, to: body.count, by: 4).map {
                Float(bitPattern: bytes.loadUnaligned(fromByteOffset: $0, as: UInt32.self).littleEndian)
            }
        }
        guard samples.allSatisfy({ $0.isFinite }) else { throw FrameError.invalidSamples }
        self.id = header.id
        self.samples = samples
    }
}

// FileHandle.read(upToCount:) can return a short read on pipes.
public func readExactly(_ input: FileHandle, count: Int, allowEOF: Bool = false) throws -> Data? {
    var data = Data()
    while data.count < count {
        guard let next = try input.read(upToCount: count - data.count), !next.isEmpty else {
            if allowEOF && data.isEmpty { return nil }
            throw FrameError.truncated
        }
        data.append(next)
    }
    return data
}
