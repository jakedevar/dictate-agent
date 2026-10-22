import Foundation

/// Collects 16 kHz mono samples for one clip, plus an optional pre-roll ring
/// that is filled while no clip is recording. With the microphone kept warm,
/// the last `preRollCapacity` samples before the hotkey went down become the
/// start of the clip, so a word spoken right at key-down is not clipped.
///
/// Not thread-safe: the audio-thread owner wraps it in a lock.
public struct ClipAccumulator: Sendable {
    /// Pre-roll kept while idle, in samples (0 disables the ring).
    public let preRollCapacity: Int
    /// The clip cap, in samples, counting the pre-roll.
    public let maxSamples: Int

    private var ring: [Float]
    private var ringHead = 0  // index of the oldest sample
    private var ringCount = 0
    private var clip: [Float] = []
    private var limitReported = false
    public private(set) var isRecording = false

    public init(preRollSamples: Int, maxSamples: Int) {
        preRollCapacity = max(0, preRollSamples)
        self.maxSamples = max(0, maxSamples)
        ring = [Float](repeating: 0, count: preRollCapacity)
    }

    /// Samples currently held in the idle ring.
    public var preRollCount: Int { ringCount }
    /// Samples in the clip so far.
    public var clipCount: Int { clip.count }

    /// Feed converted samples. While recording they go to the clip (up to
    /// `maxSamples`); while idle they go to the ring, overwriting the oldest.
    /// Returns true exactly once per clip: when the clip reaches the cap.
    @discardableResult
    public mutating func append<C: Collection>(_ chunk: C) -> Bool where C.Element == Float {
        if isRecording {
            let room = maxSamples - clip.count
            if room > 0 { clip.append(contentsOf: chunk.prefix(room)) }
            if clip.count >= maxSamples && !limitReported {
                limitReported = true
                return true
            }
            return false
        }
        guard preRollCapacity > 0 else { return false }
        for sample in chunk {
            let tail = (ringHead + ringCount) % preRollCapacity
            ring[tail] = sample
            if ringCount < preRollCapacity {
                ringCount += 1
            } else {
                ringHead = (ringHead + 1) % preRollCapacity
            }
        }
        return false
    }

    /// Start a clip seeded with the ring's contents (oldest first). Returns
    /// how many pre-roll samples the clip starts with. A no-op while already
    /// recording.
    @discardableResult
    public mutating func beginClip() -> Int {
        guard !isRecording else { return 0 }
        clip.removeAll(keepingCapacity: true)
        clip.reserveCapacity(max(clip.capacity, 16_000 * 10))
        let seed = min(ringCount, maxSamples)
        let skip = ringCount - seed
        for i in 0..<seed {
            clip.append(ring[(ringHead + skip + i) % preRollCapacity])
        }
        ringHead = 0
        ringCount = 0
        limitReported = false
        isRecording = true
        return seed
    }

    /// End the clip and return its samples. The ring starts empty again, so
    /// audio from this clip never leaks into the next one's pre-roll.
    public mutating func endClip() -> [Float] {
        guard isRecording else { return [] }
        isRecording = false
        let out = clip
        clip = []
        ringHead = 0
        ringCount = 0
        return out
    }
}
