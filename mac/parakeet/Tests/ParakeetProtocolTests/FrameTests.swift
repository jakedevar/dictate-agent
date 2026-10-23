import Foundation
import Testing
@testable import ParakeetProtocol

func bytes(_ n: UInt32) -> Data { withUnsafeBytes(of: n.littleEndian) { Data($0) } }
func body(_ header: String = "{\"id\":42,\"sample_rate\":16000}", _ pcm: Data = bytes(Float(0.5).bitPattern)) -> Data {
    bytes(UInt32(header.utf8.count)) + Data(header.utf8) + pcm
}
@Test func validFrame() throws {
    let frame = try Frame(body: body())
    #expect(frame.id == 42)
    #expect(frame.samples == [0.5])
    #expect(try Frame.size(bytes(128)) == 128)
}
@Test func rejectUnboundedAndTruncatedFrames() {
    for data in [Data(), bytes(3), bytes(UInt32.max), bytes(UInt32(Frame.maxBytes + 1))] {
        #expect(throws: (any Error).self) { try Frame.size(data) }
    }
    for data in [Data(), bytes(4097), body("{}"), body("{\"id\":1,\"sample_rate\":8000}"), body("{\"id\":1,\"sample_rate\":16000}", Data([0]))] {
        #expect(throws: (any Error).self) { try Frame(body: data) }
    }
}
@Test func rejectNonfiniteAndTooLongPCM() {
    for pcm in [bytes(Float.nan.bitPattern), bytes(Float.infinity.bitPattern), Data(count: (Frame.maxSamples + 1) * 4)] {
        #expect(throws: (any Error).self) { try Frame(body: body("{\"id\":1,\"sample_rate\":16000}", pcm)) }
    }
}
@Test func exactReadsAndEOF() throws {
    let pipe = Pipe()
    try pipe.fileHandleForWriting.write(contentsOf: Data([1, 2, 3]))
    try pipe.fileHandleForWriting.close()
    #expect(try readExactly(pipe.fileHandleForReading, count: 2) == Data([1, 2]))
    #expect(throws: (any Error).self) { try readExactly(pipe.fileHandleForReading, count: 2) }
    #expect(try readExactly(pipe.fileHandleForReading, count: 4, allowEOF: true) == nil)
}
