//! The binary frame convention for streamed audio.
//!
//! Defined now, consumed by S33 later. Audio does **not** ride as base64 JSON
//! on a streaming connection: at 16kHz mono f32 a 20ms chunk is 1280 bytes, and
//! base64-in-JSON would inflate that by a third and cost a parse per chunk.
//!
//! # Layout
//!
//! Little-endian throughout, 20-byte fixed header followed by the payload:
//!
//! ```text
//! offset  size  field
//!      0     4  magic        = "DCTA"
//!      4     1  version      = FRAME_VERSION
//!      5     1  kind         see FrameKind
//!      6     2  flags        bit 0 = LAST
//!      8     4  stream_id    correlates with AudioSource::Stream
//!     12     4  seq          per-stream, starts at 0, monotonic
//!     16     4  payload_len  bytes following the header
//!     20   ...  payload
//! ```
//!
//! # Why these fields and not others
//!
//! Sample rate, channel count, and encoding are **absent by design** — they are
//! declared once in the JSON [`Command::BeginAudioStream`](crate::Command::BeginAudioStream)
//! that opens the stream. Repeating them 50 times a second would be pure
//! overhead, and worse, would make it possible for a stream to contradict its
//! own declaration mid-flight.
//!
//! `payload_len` is explicit rather than implied by the transport, so the same
//! encoding works on a WebSocket (where message boundaries are given) and on
//! the daemon's unix socket (where they are not). See [`AudioFrame::decode`]
//! versus [`AudioFrame::decode_prefix`].
//!
//! `seq` lets a receiver detect loss or reordering. It is per-stream and starts
//! at zero.
//!
//! # Versioning
//!
//! [`FRAME_VERSION`] is bumped independently of
//! [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION): the binary layout and the JSON
//! schema evolve on separate schedules. An unknown [`FrameKind`] is *not* an
//! error — because `payload_len` is explicit, a receiver can skip a frame kind
//! it does not understand and keep the stream synchronized.

use serde::{Deserialize, Serialize};

/// Magic bytes opening every frame: `"DCTA"`.
pub const FRAME_MAGIC: [u8; 4] = *b"DCTA";

/// Version of the binary frame layout. Independent of the JSON protocol
/// version.
pub const FRAME_VERSION: u8 = 1;

/// Size of the fixed header, in bytes.
pub const FRAME_HEADER_LEN: usize = 20;

/// Largest payload a conforming receiver must accept, in bytes (1 MiB).
///
/// Exists so a decoder can reject a hostile `payload_len` *before* allocating
/// for it. At the native format this is ~16 seconds of audio in one frame,
/// far above the ~20ms chunks a live stream actually sends.
pub const MAX_FRAME_PAYLOAD_BYTES: u32 = 1024 * 1024;

/// Flag bit marking the final frame of a stream.
pub const FLAG_LAST: u16 = 1 << 0;

/// What a frame carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    /// Audio samples, in the format declared when the stream was opened.
    Audio,
    /// End of stream: transcribe what has been sent. Payload is empty.
    End,
    /// Abandon the stream and discard its audio. Payload is empty.
    Abort,
    /// A kind this build does not recognize. Because the header carries an
    /// explicit length, such a frame can be skipped without desynchronizing.
    Unknown(u8),
}

impl FrameKind {
    /// The `kind` byte for this variant.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Audio => 1,
            Self::End => 2,
            Self::Abort => 3,
            Self::Unknown(v) => v,
        }
    }

    /// Interpret a `kind` byte.
    #[must_use]
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Audio,
            2 => Self::End,
            3 => Self::Abort,
            other => Self::Unknown(other),
        }
    }

    /// Whether this build understands the kind.
    #[must_use]
    pub fn is_known(self) -> bool {
        !matches!(self, Self::Unknown(_))
    }

    /// Whether this kind ends the stream.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::End | Self::Abort)
    }
}

/// One binary frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    /// What this frame carries.
    pub kind: FrameKind,
    /// Bit flags; see [`FLAG_LAST`].
    pub flags: u16,
    /// Stream this frame belongs to.
    pub stream_id: u32,
    /// Position within the stream, from zero.
    pub seq: u32,
    /// Frame payload. Empty for [`FrameKind::End`] and [`FrameKind::Abort`].
    pub payload: Vec<u8>,
}

impl AudioFrame {
    /// An audio frame carrying samples.
    #[must_use]
    pub fn audio(stream_id: u32, seq: u32, payload: Vec<u8>) -> Self {
        Self {
            kind: FrameKind::Audio,
            flags: 0,
            stream_id,
            seq,
            payload,
        }
    }

    /// The terminating frame of a stream.
    #[must_use]
    pub fn end(stream_id: u32, seq: u32) -> Self {
        Self {
            kind: FrameKind::End,
            flags: FLAG_LAST,
            stream_id,
            seq,
            payload: Vec::new(),
        }
    }

    /// A frame abandoning a stream.
    #[must_use]
    pub fn abort(stream_id: u32, seq: u32) -> Self {
        Self {
            kind: FrameKind::Abort,
            flags: FLAG_LAST,
            stream_id,
            seq,
            payload: Vec::new(),
        }
    }

    /// Whether the final-frame flag is set.
    #[must_use]
    pub fn is_last(&self) -> bool {
        self.flags & FLAG_LAST != 0
    }

    /// Total encoded size of this frame in bytes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        FRAME_HEADER_LEN + self.payload.len()
    }

    /// Encode into a fresh buffer.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.encoded_len());
        self.encode_into(&mut buf);
        buf
    }

    /// Append the encoded frame to an existing buffer.
    ///
    /// Preferred on a hot streaming path: a live session emits ~50 frames a
    /// second and this avoids an allocation per frame.
    pub fn encode_into(&self, buf: &mut Vec<u8>) {
        buf.reserve(self.encoded_len());
        buf.extend_from_slice(&FRAME_MAGIC);
        buf.push(FRAME_VERSION);
        buf.push(self.kind.as_u8());
        buf.extend_from_slice(&self.flags.to_le_bytes());
        buf.extend_from_slice(&self.stream_id.to_le_bytes());
        buf.extend_from_slice(&self.seq.to_le_bytes());
        // Truncation is impossible in practice and would corrupt the stream, so
        // saturate rather than wrap: an over-long payload is rejected by
        // decode() as PayloadTooLarge instead of silently mis-framing.
        let len = u32::try_from(self.payload.len()).unwrap_or(u32::MAX);
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&self.payload);
    }

    /// Decode a buffer holding **exactly one** frame.
    ///
    /// This is the WebSocket path, where the transport already delimits
    /// messages. Trailing bytes are an error, because on a message-framed
    /// transport they indicate a malformed sender rather than a second frame.
    pub fn decode(buf: &[u8]) -> Result<Self, FrameError> {
        let (frame, used) = Self::decode_prefix(buf)?;
        if used != buf.len() {
            return Err(FrameError::TrailingBytes {
                extra: buf.len() - used,
            });
        }
        Ok(frame)
    }

    /// Decode the first frame from a buffer that may hold several, returning
    /// the frame and how many bytes it consumed.
    ///
    /// This is the byte-stream path, for the daemon's unix socket. Call in a
    /// loop, advancing by the returned count; [`FrameError::Incomplete`] means
    /// "read more from the socket and try again", and carries how many bytes
    /// are still needed.
    pub fn decode_prefix(buf: &[u8]) -> Result<(Self, usize), FrameError> {
        if buf.len() < FRAME_HEADER_LEN {
            return Err(FrameError::Incomplete {
                needed: FRAME_HEADER_LEN - buf.len(),
            });
        }
        if buf[0..4] != FRAME_MAGIC {
            return Err(FrameError::BadMagic {
                found: [buf[0], buf[1], buf[2], buf[3]],
            });
        }
        let version = buf[4];
        if version != FRAME_VERSION {
            return Err(FrameError::UnsupportedVersion { found: version });
        }
        let kind = FrameKind::from_u8(buf[5]);
        let flags = u16::from_le_bytes([buf[6], buf[7]]);
        let stream_id = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let seq = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
        let payload_len = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);

        // Bound-check before allocating: payload_len is attacker-controlled.
        if payload_len > MAX_FRAME_PAYLOAD_BYTES {
            return Err(FrameError::PayloadTooLarge {
                len: payload_len,
                max: MAX_FRAME_PAYLOAD_BYTES,
            });
        }
        let payload_len = payload_len as usize;
        let total = FRAME_HEADER_LEN + payload_len;
        if buf.len() < total {
            return Err(FrameError::Incomplete {
                needed: total - buf.len(),
            });
        }

        Ok((
            Self {
                kind,
                flags,
                stream_id,
                seq,
                payload: buf[FRAME_HEADER_LEN..total].to_vec(),
            },
            total,
        ))
    }

    /// Decode every complete frame in `buf`, returning them and the number of
    /// bytes consumed. A trailing partial frame is left unconsumed rather than
    /// treated as an error.
    pub fn decode_all(buf: &[u8]) -> Result<(Vec<Self>, usize), FrameError> {
        let mut frames = Vec::new();
        let mut offset = 0usize;
        loop {
            match Self::decode_prefix(&buf[offset..]) {
                Ok((f, used)) => {
                    frames.push(f);
                    offset += used;
                }
                Err(FrameError::Incomplete { .. }) => return Ok((frames, offset)),
                Err(e) => return Err(e),
            }
        }
    }
}

/// Why a frame could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// More bytes are required before this frame can be decoded. Not fatal on a
    /// stream transport: read more and retry.
    Incomplete {
        /// How many further bytes are needed.
        needed: usize,
    },
    /// The frame did not begin with [`FRAME_MAGIC`]; the stream is
    /// desynchronized or the sender is not speaking this protocol.
    BadMagic {
        /// The four bytes actually found.
        found: [u8; 4],
    },
    /// The frame layout version is not one this build can decode.
    UnsupportedVersion {
        /// The version byte found.
        found: u8,
    },
    /// The declared payload length exceeds [`MAX_FRAME_PAYLOAD_BYTES`].
    PayloadTooLarge {
        /// The declared length.
        len: u32,
        /// The accepted maximum.
        max: u32,
    },
    /// A single-frame decode found bytes after the frame.
    TrailingBytes {
        /// How many bytes were left over.
        extra: usize,
    },
}

impl core::fmt::Display for FrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Incomplete { needed } => {
                write!(f, "incomplete frame: {needed} more byte(s) needed")
            }
            Self::BadMagic { found } => {
                write!(
                    f,
                    "bad frame magic: expected {FRAME_MAGIC:?}, found {found:?}"
                )
            }
            Self::UnsupportedVersion { found } => write!(
                f,
                "unsupported frame version {found}, this build speaks {FRAME_VERSION}"
            ),
            Self::PayloadTooLarge { len, max } => {
                write!(f, "frame payload {len} exceeds maximum {max}")
            }
            Self::TrailingBytes { extra } => {
                write!(f, "{extra} unexpected byte(s) after frame")
            }
        }
    }
}

impl std::error::Error for FrameError {}

impl From<FrameError> for crate::error::ProtoError {
    fn from(e: FrameError) -> Self {
        let code = match e {
            FrameError::PayloadTooLarge { .. } => crate::error::ErrorCode::PayloadTooLarge,
            FrameError::UnsupportedVersion { .. } => crate::error::ErrorCode::UnsupportedVersion,
            _ => crate::error::ErrorCode::MalformedRequest,
        };
        Self::new(code, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_frame_round_trips() {
        let f = AudioFrame::audio(42, 7, vec![1, 2, 3, 4, 5]);
        let bytes = f.encode();
        assert_eq!(bytes.len(), FRAME_HEADER_LEN + 5);
        assert_eq!(AudioFrame::decode(&bytes).unwrap(), f);
    }

    #[test]
    fn header_layout_is_pinned() {
        // Golden bytes: this test is the binary equivalent of the JSON goldens.
        // A change here is a FRAME_VERSION bump, never a silent edit.
        let f = AudioFrame {
            kind: FrameKind::Audio,
            flags: FLAG_LAST,
            stream_id: 0x0102_0304,
            seq: 0x0506_0708,
            payload: vec![0xAA, 0xBB],
        };
        let b = f.encode();
        assert_eq!(
            b,
            vec![
                b'D', b'C', b'T', b'A', // magic
                1,    // version
                1,    // kind = Audio
                0x01, 0x00, // flags = LAST (LE)
                0x04, 0x03, 0x02, 0x01, // stream_id (LE)
                0x08, 0x07, 0x06, 0x05, // seq (LE)
                0x02, 0x00, 0x00, 0x00, // payload_len (LE)
                0xAA, 0xBB, // payload
            ]
        );
        assert_eq!(AudioFrame::decode(&b).unwrap(), f);
    }

    #[test]
    fn empty_control_frames_round_trip() {
        for f in [AudioFrame::end(1, 99), AudioFrame::abort(1, 99)] {
            let b = f.encode();
            assert_eq!(b.len(), FRAME_HEADER_LEN);
            let d = AudioFrame::decode(&b).unwrap();
            assert_eq!(d, f);
            assert!(d.is_last());
            assert!(d.kind.is_terminal());
        }
    }

    #[test]
    fn truncated_header_reports_how_much_more_is_needed() {
        let b = AudioFrame::audio(1, 0, vec![9; 4]).encode();
        assert_eq!(
            AudioFrame::decode(&b[..10]),
            Err(FrameError::Incomplete { needed: 10 })
        );
        assert_eq!(
            AudioFrame::decode(&[]),
            Err(FrameError::Incomplete { needed: 20 })
        );
    }

    #[test]
    fn truncated_payload_reports_how_much_more_is_needed() {
        let b = AudioFrame::audio(1, 0, vec![9; 100]).encode();
        assert_eq!(
            AudioFrame::decode(&b[..FRAME_HEADER_LEN + 60]),
            Err(FrameError::Incomplete { needed: 40 })
        );
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut b = AudioFrame::audio(1, 0, vec![]).encode();
        b[0] = b'X';
        assert_eq!(
            AudioFrame::decode(&b),
            Err(FrameError::BadMagic {
                found: [b'X', b'C', b'T', b'A']
            })
        );
    }

    #[test]
    fn wrong_frame_version_is_rejected() {
        let mut b = AudioFrame::audio(1, 0, vec![]).encode();
        b[4] = 99;
        assert_eq!(
            AudioFrame::decode(&b),
            Err(FrameError::UnsupportedVersion { found: 99 })
        );
    }

    /// A hostile length must be refused before any allocation happens.
    #[test]
    fn oversized_payload_length_is_rejected_without_allocating() {
        let mut b = AudioFrame::audio(1, 0, vec![]).encode();
        b[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            AudioFrame::decode(&b),
            Err(FrameError::PayloadTooLarge {
                len: u32::MAX,
                max: MAX_FRAME_PAYLOAD_BYTES
            })
        );
    }

    #[test]
    fn trailing_bytes_rejected_on_message_transport_but_not_on_stream() {
        let mut b = AudioFrame::audio(1, 0, vec![7]).encode();
        b.push(0xFF);
        assert_eq!(
            AudioFrame::decode(&b),
            Err(FrameError::TrailingBytes { extra: 1 })
        );
        // The stream path stops cleanly at the frame boundary instead.
        let (f, used) = AudioFrame::decode_prefix(&b).unwrap();
        assert_eq!(f.payload, vec![7]);
        assert_eq!(used, FRAME_HEADER_LEN + 1);
    }

    /// The property that makes the UDS path work: several frames concatenated
    /// in one read must decode individually.
    #[test]
    fn concatenated_frames_decode_in_sequence() {
        let mut buf = Vec::new();
        for seq in 0..3u32 {
            AudioFrame::audio(5, seq, vec![seq as u8; 8]).encode_into(&mut buf);
        }
        AudioFrame::end(5, 3).encode_into(&mut buf);

        let (frames, used) = AudioFrame::decode_all(&buf).unwrap();
        assert_eq!(used, buf.len());
        assert_eq!(frames.len(), 4);
        assert_eq!(frames[2].seq, 2);
        assert_eq!(frames[2].payload, vec![2u8; 8]);
        assert!(frames[3].kind.is_terminal());
    }

    #[test]
    fn partial_trailing_frame_is_left_unconsumed() {
        let mut buf = AudioFrame::audio(1, 0, vec![1, 2, 3]).encode();
        let whole = buf.len();
        buf.extend_from_slice(&AudioFrame::audio(1, 1, vec![4; 50]).encode()[..30]);

        let (frames, used) = AudioFrame::decode_all(&buf).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(used, whole, "the partial frame must stay in the buffer");
    }

    /// Forward compat: an explicit length means an unknown kind can be skipped
    /// without losing stream synchronization.
    #[test]
    fn unknown_frame_kind_decodes_and_keeps_the_stream_aligned() {
        let mut b = AudioFrame::audio(1, 0, vec![1, 2, 3]).encode();
        b[5] = 77;
        let mut buf = b;
        AudioFrame::audio(1, 1, vec![9]).encode_into(&mut buf);

        let (frames, used) = AudioFrame::decode_all(&buf).unwrap();
        assert_eq!(used, buf.len());
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].kind, FrameKind::Unknown(77));
        assert!(!frames[0].kind.is_known());
        // The frame after the unrecognized one is still read correctly.
        assert_eq!(frames[1].kind, FrameKind::Audio);
        assert_eq!(frames[1].payload, vec![9]);
    }

    #[test]
    fn max_size_payload_is_accepted() {
        let f = AudioFrame::audio(1, 0, vec![0; MAX_FRAME_PAYLOAD_BYTES as usize]);
        let b = f.encode();
        assert_eq!(AudioFrame::decode(&b).unwrap(), f);
    }

    #[test]
    fn frame_errors_map_onto_protocol_errors() {
        use crate::error::ErrorCode;
        let e: crate::error::ProtoError = FrameError::PayloadTooLarge {
            len: 99,
            max: MAX_FRAME_PAYLOAD_BYTES,
        }
        .into();
        assert_eq!(e.code, ErrorCode::PayloadTooLarge);

        let e: crate::error::ProtoError = FrameError::BadMagic { found: [0; 4] }.into();
        assert_eq!(e.code, ErrorCode::MalformedRequest);
    }

    #[test]
    fn kind_byte_mapping_is_stable() {
        assert_eq!(FrameKind::Audio.as_u8(), 1);
        assert_eq!(FrameKind::End.as_u8(), 2);
        assert_eq!(FrameKind::Abort.as_u8(), 3);
        assert_eq!(FrameKind::from_u8(1), FrameKind::Audio);
        assert_eq!(FrameKind::from_u8(200), FrameKind::Unknown(200));
    }
}
