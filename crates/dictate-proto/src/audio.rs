//! Audio formats and the three ways a client can hand audio to the server.

use serde::{Deserialize, Serialize};

open_str_enum! {
    /// Sample encoding.
    ///
    /// `PcmF32Le` is the daemon's native pipeline format (what cpal captures and
    /// what whisper.cpp consumes), so it is the zero-conversion path. The others
    /// exist because a thin client should not be required to resample.
    pub enum AudioEncoding {
        /// Raw 32-bit little-endian float samples, interleaved.
        PcmF32Le => "pcm_f32le",
        /// Raw 16-bit little-endian signed integer samples, interleaved.
        PcmS16Le => "pcm_s16le",
        /// A complete RIFF/WAVE container; rate and channel count come from its
        /// header, so [`AudioFormat`] may omit them.
        Wav => "wav",
    }
    default = PcmF32Le;
}

impl AudioEncoding {
    /// Bytes per sample per channel, or `None` for container formats whose
    /// layout is described by their own header.
    #[must_use]
    pub fn bytes_per_sample(&self) -> Option<u32> {
        match self {
            Self::PcmF32Le => Some(4),
            Self::PcmS16Le => Some(2),
            Self::Wav | Self::Unknown(_) => None,
        }
    }

    /// Whether this encoding is a self-describing container rather than raw
    /// samples.
    #[must_use]
    pub fn is_container(&self) -> bool {
        matches!(self, Self::Wav)
    }
}

/// The sample rate whisper.cpp requires; anything else must be resampled.
pub const WHISPER_SAMPLE_RATE_HZ: u32 = 16_000;

/// A description of an audio payload's layout.
///
/// Declared **once** per upload or per stream — never per binary frame. Keeping
/// format out of the frame header is what lets [`crate::frame::AudioFrame`] use
/// a fixed, tiny header on 20ms chunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Sample encoding.
    #[serde(default)]
    pub encoding: AudioEncoding,

    /// Samples per second. Optional for container encodings, which carry it in
    /// their own header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate_hz: Option<u32>,

    /// Channel count. Optional for container encodings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self::whisper_native()
    }
}

impl AudioFormat {
    /// The pipeline's native format: 16kHz mono 32-bit float.
    #[must_use]
    pub fn whisper_native() -> Self {
        Self {
            encoding: AudioEncoding::PcmF32Le,
            sample_rate_hz: Some(WHISPER_SAMPLE_RATE_HZ),
            channels: Some(1),
        }
    }

    /// A WAV upload, whose parameters come from the file header.
    #[must_use]
    pub fn wav() -> Self {
        Self {
            encoding: AudioEncoding::Wav,
            sample_rate_hz: None,
            channels: None,
        }
    }

    /// Bytes occupied by one sample across all channels, for raw encodings.
    #[must_use]
    pub fn bytes_per_frame(&self) -> Option<u32> {
        Some(self.encoding.bytes_per_sample()? * u32::from(self.channels?))
    }

    /// Duration of `byte_len` bytes of raw audio in this format.
    ///
    /// `None` for container encodings and when rate or channel count is
    /// unspecified — the caller must parse the container instead of guessing.
    #[must_use]
    pub fn duration_ms(&self, byte_len: usize) -> Option<f64> {
        let per_frame = self.bytes_per_frame()?;
        let rate = self.sample_rate_hz?;
        if per_frame == 0 || rate == 0 {
            return None;
        }
        let frames = byte_len as f64 / f64::from(per_frame);
        Some(frames * 1000.0 / f64::from(rate))
    }

    /// Whether this format can be fed to the STT stage without resampling.
    #[must_use]
    pub fn is_whisper_native(&self) -> bool {
        self.encoding == AudioEncoding::PcmF32Le
            && self.sample_rate_hz == Some(WHISPER_SAMPLE_RATE_HZ)
            && self.channels == Some(1)
    }
}

/// Where the audio for a transcription request comes from.
///
/// Three variants because there are three real transports, and collapsing them
/// would force one of the three to carry audio awkwardly:
///
/// | Variant | Transport |
/// |---|---|
/// | [`AudioSource::Inline`] | a single JSON request — the simplest thing a phone client can do |
/// | [`AudioSource::Stream`] | binary frames on an open WebSocket, referenced by stream id |
/// | [`AudioSource::Body`] | `POST /v1/transcribe` where bytes are the HTTP body and this JSON is only parameters |
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum AudioSource {
    /// Audio bytes carried inside this message, base64-encoded.
    ///
    /// Convenient and self-contained, but ~33% larger on the wire than the
    /// binary paths; honor [`Limits::max_message_bytes`](crate::Limits::max_message_bytes).
    Inline {
        /// Layout of the decoded bytes.
        format: AudioFormat,
        /// The audio, base64 (standard alphabet, with padding).
        #[serde(with = "b64")]
        data: Vec<u8>,
    },

    /// Audio arriving as binary frames on this connection.
    Stream {
        /// Stream identifier shared with [`crate::frame::AudioFrame::stream_id`].
        stream_id: u32,
    },

    /// Audio supplied out-of-band by the transport, e.g. an HTTP request body
    /// or a multipart part.
    Body {
        /// Layout of those bytes. May be [`AudioEncoding::Wav`] to let the
        /// container describe itself.
        #[serde(default)]
        format: AudioFormat,
    },
}

impl AudioSource {
    /// Inline audio in the pipeline's native format.
    #[must_use]
    pub fn inline_native(data: Vec<u8>) -> Self {
        Self::Inline {
            format: AudioFormat::whisper_native(),
            data,
        }
    }

    /// The declared format, when this source declares one.
    #[must_use]
    pub fn format(&self) -> Option<&AudioFormat> {
        match self {
            Self::Inline { format, .. } | Self::Body { format } => Some(format),
            Self::Stream { .. } => None,
        }
    }
}

/// base64 (de)serialization for inline audio.
mod b64 {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_audio_rides_as_base64_not_a_number_array() {
        let src = AudioSource::inline_native(vec![0xde, 0xad, 0xbe, 0xef]);
        let json = serde_json::to_string(&src).unwrap();
        assert!(json.contains(r#""data":"3q2+7w==""#), "{json}");
        assert!(!json.contains("222"), "must not encode bytes as numbers");
        assert_eq!(serde_json::from_str::<AudioSource>(&json).unwrap(), src);
    }

    #[test]
    fn malformed_base64_is_a_deserialization_error() {
        let json = r#"{"source":"inline","format":{"encoding":"pcm_f32le"},"data":"not!base64!"}"#;
        assert!(serde_json::from_str::<AudioSource>(json).is_err());
    }

    #[test]
    fn duration_math_for_raw_formats() {
        let f = AudioFormat::whisper_native();
        assert_eq!(f.bytes_per_frame(), Some(4));
        // 16000 frames * 4 bytes = 1s of audio.
        assert_eq!(f.duration_ms(64_000), Some(1000.0));
        assert!(f.is_whisper_native());

        let s16 = AudioFormat {
            encoding: AudioEncoding::PcmS16Le,
            sample_rate_hz: Some(16_000),
            channels: Some(2),
        };
        assert_eq!(s16.bytes_per_frame(), Some(4));
        assert!(!s16.is_whisper_native());
    }

    #[test]
    fn container_formats_refuse_to_guess_duration() {
        let w = AudioFormat::wav();
        assert_eq!(w.bytes_per_frame(), None);
        assert_eq!(w.duration_ms(64_000), None);
        assert!(w.encoding.is_container());
    }

    #[test]
    fn stream_source_declares_no_format() {
        let s = AudioSource::Stream { stream_id: 7 };
        assert!(s.format().is_none());
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"source":"stream","stream_id":7}"#);
    }

    #[test]
    fn body_source_defaults_to_native_format() {
        let s: AudioSource = serde_json::from_str(r#"{"source":"body"}"#).unwrap();
        assert_eq!(
            s,
            AudioSource::Body {
                format: AudioFormat::whisper_native()
            }
        );
    }

    #[test]
    fn unknown_encoding_round_trips() {
        let f: AudioFormat = serde_json::from_str(r#"{"encoding":"opus"}"#).unwrap();
        assert_eq!(f.encoding, AudioEncoding::Unknown("opus".into()));
        assert_eq!(f.encoding.bytes_per_sample(), None);
        assert!(serde_json::to_string(&f).unwrap().contains("opus"));
    }

    #[test]
    fn empty_inline_audio_round_trips() {
        let src = AudioSource::inline_native(Vec::new());
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(serde_json::from_str::<AudioSource>(&json).unwrap(), src);
    }
}
