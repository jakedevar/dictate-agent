//! Turning a `transcribe_audio` payload into pipeline samples.
//!
//! Everything a client can get wrong about an upload is caught *here*, before
//! a session exists: an oversized clip, a truncated WAV, an unsupported
//! encoding, a raw stream with no declared sample rate. Those are request
//! errors, answered with a precise [`ProtoError`], and they never occupy the
//! engine's single session slot — a bad upload must not make a good dictation
//! answer `busy`.
//!
//! The DSP itself lives in [`dictate_audio::decode`]; this module is the seam
//! that maps its errors onto the protocol's vocabulary.

use std::time::Instant;

use dictate_audio::decode::{self, DecodeError, RawEncoding};
use dictate_proto::{AudioEncoding, AudioSource, ErrorCode, Limits, ProtoError};

use crate::session::SuppliedAudio;

/// Decode an upload into 16 kHz mono samples, enforcing `limits`.
///
/// CPU-bound (a long clip at 48 kHz stereo is tens of milliseconds of
/// resampling); call it from the blocking pool.
///
/// # Errors
///
/// - `capability_unavailable` for `stream`/`body` sources, which belong to the
///   network transports (S33);
/// - `invalid_params` for empty audio or a raw stream missing its layout;
/// - `payload_too_large` when the payload or its duration exceeds `limits`;
/// - `audio_format_unsupported` for anything undecodable.
pub fn decode_upload(audio: &AudioSource, limits: &Limits) -> Result<SuppliedAudio, ProtoError> {
    let AudioSource::Inline { format, data } = audio else {
        return Err(ProtoError::new(
            ErrorCode::CapabilityUnavailable,
            "the local socket accepts inline audio only; \
             `stream` and `body` sources belong to the network API",
        ));
    };
    if data.is_empty() {
        return Err(ProtoError::new(
            ErrorCode::InvalidParams,
            "the audio payload is empty",
        ));
    }
    if data.len() as u64 > u64::from(limits.max_message_bytes) {
        return Err(too_large_bytes(data.len() as u64, limits));
    }

    let max_ms = limits.max_audio_ms.map(u64::from);
    let started = Instant::now();
    let decoded = match &format.encoding {
        AudioEncoding::Wav => decode::decode_wav(data, max_ms),
        AudioEncoding::PcmS16Le => decode::decode_raw(
            data,
            RawEncoding::S16Le,
            format.sample_rate_hz,
            format.channels,
            max_ms,
        ),
        AudioEncoding::PcmF32Le => decode::decode_raw(
            data,
            RawEncoding::F32Le,
            format.sample_rate_hz,
            format.channels,
            max_ms,
        ),
        AudioEncoding::Unknown(name) => {
            return Err(ProtoError::new(
                ErrorCode::AudioFormatUnsupported,
                format!("unsupported audio encoding '{name}'; use wav, pcm_s16le or pcm_f32le"),
            ))
        }
    }
    .map_err(map_error)?;

    Ok(SuppliedAudio {
        samples: decoded.samples,
        prepare_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

fn too_large_bytes(actual: u64, limits: &Limits) -> ProtoError {
    ProtoError::new(
        ErrorCode::PayloadTooLarge,
        format!(
            "the audio payload is {actual} bytes; this connection accepts at most {}",
            limits.max_message_bytes
        ),
    )
    .with_detail(serde_json::json!({
        "limit_bytes": limits.max_message_bytes,
        "actual_bytes": actual,
    }))
}

fn map_error(error: DecodeError) -> ProtoError {
    match error {
        DecodeError::TooLong {
            duration_ms,
            max_ms,
        } => ProtoError::new(ErrorCode::PayloadTooLarge, error.to_string())
            .with_detail(serde_json::json!({ "limit_ms": max_ms, "actual_ms": duration_ms })),
        DecodeError::MissingParameter(_) => {
            ProtoError::new(ErrorCode::InvalidParams, error.to_string())
        }
        DecodeError::Malformed(_) | DecodeError::Unsupported(_) => {
            ProtoError::new(ErrorCode::AudioFormatUnsupported, error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dictate_proto::AudioFormat;

    fn inline(format: AudioFormat, data: Vec<u8>) -> AudioSource {
        AudioSource::Inline { format, data }
    }

    fn s16_mono_16k(frames: usize) -> Vec<u8> {
        vec![0u8; frames * 2]
    }

    fn pcm16(rate: u32, channels: u16) -> AudioFormat {
        AudioFormat {
            encoding: AudioEncoding::PcmS16Le,
            sample_rate_hz: Some(rate),
            channels: Some(channels),
        }
    }

    fn code(r: Result<SuppliedAudio, ProtoError>) -> ErrorCode {
        r.expect_err("expected an error").code
    }

    #[test]
    fn a_native_upload_decodes_and_reports_what_preparing_it_cost() {
        let a = decode_upload(
            &inline(pcm16(16_000, 1), s16_mono_16k(16_000)),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(a.samples.len(), 16_000);
        assert!(a.prepare_ms >= 0.0);
    }

    #[test]
    fn a_stereo_44k_upload_arrives_as_16k_mono() {
        let frames = 44_100;
        let a = decode_upload(
            &inline(pcm16(44_100, 2), vec![0u8; frames * 4]),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(a.samples.len(), 16_000);
    }

    #[test]
    fn stream_and_body_sources_are_not_available_on_the_local_socket() {
        for source in [
            AudioSource::Stream { stream_id: 1 },
            AudioSource::Body {
                format: AudioFormat::wav(),
            },
        ] {
            assert_eq!(
                code(decode_upload(&source, &Limits::default())),
                ErrorCode::CapabilityUnavailable
            );
        }
    }

    #[test]
    fn empty_audio_is_an_invalid_request_not_silence() {
        assert_eq!(
            code(decode_upload(
                &inline(AudioFormat::whisper_native(), Vec::new()),
                &Limits::default()
            )),
            ErrorCode::InvalidParams
        );
    }

    #[test]
    fn an_over_long_clip_is_payload_too_large_with_the_numbers() {
        let limits = Limits {
            max_audio_ms: Some(1_000),
            ..Limits::default()
        };
        let err = decode_upload(&inline(pcm16(16_000, 1), s16_mono_16k(32_000)), &limits)
            .expect_err("2 s exceeds a 1 s limit");
        assert_eq!(err.code, ErrorCode::PayloadTooLarge);
        assert_eq!(
            err.detail().unwrap(),
            &serde_json::json!({"limit_ms": 1000, "actual_ms": 2000})
        );
    }

    #[test]
    fn an_over_large_payload_is_refused_before_decoding() {
        let limits = Limits {
            max_message_bytes: 1_000,
            ..Limits::default()
        };
        let err = decode_upload(&inline(pcm16(16_000, 1), s16_mono_16k(1_000)), &limits)
            .expect_err("2000 bytes exceeds 1000");
        assert_eq!(err.code, ErrorCode::PayloadTooLarge);
        assert_eq!(err.detail().unwrap()["limit_bytes"], 1_000);
    }

    #[test]
    fn a_malformed_wav_is_audio_format_unsupported() {
        assert_eq!(
            code(decode_upload(
                &inline(AudioFormat::wav(), b"RIFFnot really a wav".to_vec()),
                &Limits::default()
            )),
            ErrorCode::AudioFormatUnsupported
        );
    }

    #[test]
    fn raw_audio_without_a_declared_rate_is_invalid_params() {
        let format = AudioFormat {
            encoding: AudioEncoding::PcmS16Le,
            sample_rate_hz: None,
            channels: Some(1),
        };
        assert_eq!(
            code(decode_upload(
                &inline(format, s16_mono_16k(100)),
                &Limits::default()
            )),
            ErrorCode::InvalidParams
        );
    }

    #[test]
    fn an_unknown_encoding_names_the_ones_that_work() {
        let format = AudioFormat {
            encoding: AudioEncoding::Unknown("opus".into()),
            sample_rate_hz: Some(48_000),
            channels: Some(1),
        };
        let err = decode_upload(&inline(format, vec![1, 2, 3]), &Limits::default())
            .expect_err("opus is not supported");
        assert_eq!(err.code, ErrorCode::AudioFormatUnsupported);
        assert!(err.message.contains("pcm_s16le"), "{}", err.message);
    }

    #[test]
    fn a_partial_raw_frame_is_rejected_not_truncated() {
        assert_eq!(
            code(decode_upload(
                &inline(pcm16(16_000, 1), vec![0u8; 3]),
                &Limits::default()
            )),
            ErrorCode::AudioFormatUnsupported
        );
    }
}
