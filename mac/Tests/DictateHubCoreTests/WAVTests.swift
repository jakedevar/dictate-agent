import Foundation
import Testing
@testable import DictateHubCore

private func u32(_ d: Data, _ at: Int) -> UInt32 {
    d[d.startIndex + at ..< d.startIndex + at + 4].enumerated()
        .reduce(0) { $0 | UInt32($1.element) << (8 * UInt32($1.offset)) }
}
private func u16(_ d: Data, _ at: Int) -> UInt16 {
    UInt16(d[d.startIndex + at]) | UInt16(d[d.startIndex + at + 1]) << 8
}
private func ascii(_ d: Data, _ at: Int) -> String {
    String(decoding: d[d.startIndex + at ..< d.startIndex + at + 4], as: UTF8.self)
}

@Suite struct WAVTests {
    @Test func headerIsCanonical16kMonoPCM() {
        let wav = WAV.encode(samples: [0, 1, -1, 32_767, -32_768])
        #expect(wav.count == 44 + 10)
        #expect(ascii(wav, 0) == "RIFF")
        #expect(u32(wav, 4) == UInt32(36 + 10))
        #expect(ascii(wav, 8) == "WAVE")
        #expect(ascii(wav, 12) == "fmt ")
        #expect(u32(wav, 16) == 16)
        #expect(u16(wav, 20) == 1)  // PCM
        #expect(u16(wav, 22) == 1)  // mono
        #expect(u32(wav, 24) == 16_000)
        #expect(u32(wav, 28) == 32_000)  // byte rate
        #expect(u16(wav, 32) == 2)  // block align
        #expect(u16(wav, 34) == 16)
        #expect(ascii(wav, 36) == "data")
        #expect(u32(wav, 40) == 10)
    }

    @Test func samplesAreLittleEndianTwosComplement() {
        let wav = WAV.encode(samples: [1, -1, 32_767, -32_768])
        let body = [UInt8](wav.suffix(8))
        #expect(body == [0x01, 0x00, 0xFF, 0xFF, 0xFF, 0x7F, 0x00, 0x80])
    }

    @Test func emptyClipIsJustAHeader() {
        let wav = WAV.encode(samples: [])
        #expect(wav.count == 44)
        #expect(u32(wav, 40) == 0)
        #expect(u32(wav, 4) == 36)
    }

    @Test func floatConversionScalesAndRounds() {
        #expect(WAV.int16(from: 0) == 0)
        #expect(WAV.int16(from: 1) == 32_767)
        #expect(WAV.int16(from: -1) == -32_767)
        #expect(WAV.int16(from: 0.5) == 16_384)  // 16383.5 rounds away from zero
        #expect(WAV.int16(from: -0.5) == -16_384)
    }

    @Test func floatConversionClampsAndSilencesNaN() {
        #expect(WAV.int16(from: 1.7) == 32_767)
        #expect(WAV.int16(from: -3) == -32_767)
        #expect(WAV.int16(from: .infinity) == 32_767)
        #expect(WAV.int16(from: -.infinity) == -32_767)
        #expect(WAV.int16(from: .nan) == 0)
    }

    @Test func floatEncodeMatchesIntEncode() {
        let floats: [Float] = [0, 0.25, -0.25, 1, -1]
        #expect(WAV.encode(floatSamples: floats) == WAV.encode(samples: WAV.int16Samples(from: floats)))
    }

    @Test func twoMinuteCapSizeFitsTheHubUploadLimit() {
        // 120 s at 16 kHz s16 mono, against the hub's default 32 MiB cap.
        let bytes = WAV.headerSize + 120 * WAV.sampleRate * 2
        #expect(bytes == 3_840_044)
        #expect(bytes < 32 * 1024 * 1024)
        #expect(WAV.duration(sampleCount: 16_000 * 120) == 120)
    }
}
