//! Decoding caller-supplied audio into the pipeline's native format.
//!
//! The pipeline consumes exactly one shape of audio: 16 kHz, mono, `f32`
//! (what cpal captures here and what whisper.cpp consumes). Everything a
//! client may upload — WAV files at 44.1/48 kHz, stereo, 24-bit, raw PCM —
//! is normalised to that shape here, in one place, so the STT stage never has
//! to wonder what it was handed.
//!
//! # Why the resampler is a real one
//!
//! Downsampling 48 kHz to 16 kHz by dropping two of every three samples folds
//! everything above 8 kHz back into the speech band as aliasing, and Whisper
//! is measurably worse on aliased input. [`resample_mono`] is a
//! Kaiser-windowed-sinc polyphase resampler: the low-pass cutoff sits just
//! below the *output* Nyquist frequency, so out-of-band energy is removed
//! before it can alias. Its filter tables are built lazily per output phase,
//! so an awkward ratio (e.g. 44.1 kHz → 16 kHz, 160 phases) costs only the
//! phases it actually visits.
//!
//! This module is deliberately independent of `dictate-proto`: the daemon maps
//! [`DecodeError`] onto protocol errors, keeping this crate a pure DSP/decoding
//! leaf that the streaming path (S33) can reuse.

use std::io::Cursor;

use hound::{SampleFormat, WavReader};

/// The pipeline's sample rate.
pub const TARGET_SAMPLE_RATE_HZ: u32 = 16_000;

/// Lowest input sample rate accepted. Below this, "speech" is not speech.
pub const MIN_INPUT_RATE_HZ: u32 = 4_000;
/// Highest input sample rate accepted; bounds the resampler's filter length.
pub const MAX_INPUT_RATE_HZ: u32 = 192_000;
/// Most interleaved channels accepted.
pub const MAX_CHANNELS: u16 = 8;

/// How many sinc zero-crossings each side of the kernel spans. With the Kaiser
/// window below this gives a transition band narrow enough to keep speech
/// energy up to ~7 kHz within 0.5 dB while rejecting anything that would alias
/// into it.
const KERNEL_ZEROS_PER_SIDE: f64 = 32.0;
/// Kaiser window shape parameter (≈ 96 dB side-lobe attenuation).
const KAISER_BETA: f64 = 9.0;
/// Cutoff as a fraction of the output Nyquist frequency.
const CUTOFF_FRACTION: f64 = 0.95;

/// Raw, headerless sample encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawEncoding {
    /// 32-bit little-endian IEEE float.
    F32Le,
    /// 16-bit little-endian signed integer.
    S16Le,
}

impl RawEncoding {
    fn bytes_per_sample(self) -> usize {
        match self {
            Self::F32Le => 4,
            Self::S16Le => 2,
        }
    }
}

/// Why audio could not be turned into pipeline samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The container or sample data is not valid (bad header, truncated data,
    /// non-finite samples).
    Malformed(String),
    /// Valid, but not something this decoder supports (unusual sample format,
    /// sample rate or channel count out of range).
    Unsupported(String),
    /// The clip is longer than the configured limit.
    TooLong {
        /// The clip's duration.
        duration_ms: u64,
        /// The configured ceiling.
        max_ms: u64,
    },
    /// The caller left out a parameter a raw stream cannot do without.
    MissingParameter(&'static str),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(m) => write!(f, "malformed audio: {m}"),
            Self::Unsupported(m) => write!(f, "unsupported audio: {m}"),
            Self::TooLong {
                duration_ms,
                max_ms,
            } => write!(
                f,
                "audio is {:.1}s long; this daemon accepts at most {:.1}s",
                *duration_ms as f64 / 1000.0,
                *max_ms as f64 / 1000.0
            ),
            Self::MissingParameter(p) => write!(f, "raw audio requires `{p}`"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Audio normalised to the pipeline's format, plus what it was before.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// 16 kHz mono samples in `[-1.0, 1.0]`-ish (not clipped: float WAVs may
    /// legitimately exceed full scale and the VAD/STT stages tolerate it).
    pub samples: Vec<f32>,
    /// The source's sample rate.
    pub source_rate_hz: u32,
    /// The source's channel count.
    pub source_channels: u16,
}

impl Decoded {
    /// Duration of the normalised audio in milliseconds.
    #[must_use]
    pub fn duration_ms(&self) -> f64 {
        self.samples.len() as f64 * 1000.0 / f64::from(TARGET_SAMPLE_RATE_HZ)
    }
}

fn check_layout(rate_hz: u32, channels: u16) -> Result<(), DecodeError> {
    if !(MIN_INPUT_RATE_HZ..=MAX_INPUT_RATE_HZ).contains(&rate_hz) {
        return Err(DecodeError::Unsupported(format!(
            "sample rate {rate_hz} Hz is outside {MIN_INPUT_RATE_HZ}..={MAX_INPUT_RATE_HZ} Hz"
        )));
    }
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(DecodeError::Unsupported(format!(
            "{channels} channels is outside 1..={MAX_CHANNELS}"
        )));
    }
    Ok(())
}

fn check_duration(frames: u64, rate_hz: u32, max_ms: Option<u64>) -> Result<(), DecodeError> {
    let duration_ms = frames.saturating_mul(1000) / u64::from(rate_hz);
    match max_ms {
        Some(max) if duration_ms > max => Err(DecodeError::TooLong {
            duration_ms,
            max_ms: max,
        }),
        _ => Ok(()),
    }
}

/// Decode a complete RIFF/WAVE file: PCM 8/16/24/32-bit or 32-bit float, any
/// sample rate in range, any channel count in range.
///
/// The duration limit is enforced from the header **before** any sample is
/// decoded or allocated, so an oversized upload costs almost nothing.
///
/// # Errors
///
/// See [`DecodeError`].
pub fn decode_wav(bytes: &[u8], max_ms: Option<u64>) -> Result<Decoded, DecodeError> {
    let mut reader = WavReader::new(Cursor::new(bytes))
        .map_err(|e| DecodeError::Malformed(format!("not a readable WAV file: {e}")))?;
    let spec = reader.spec();
    check_layout(spec.sample_rate, spec.channels)?;
    check_duration(u64::from(reader.duration()), spec.sample_rate, max_ms)?;

    let mut interleaved: Vec<f32> = Vec::with_capacity(reader.len() as usize);
    let bad =
        |e: hound::Error| DecodeError::Malformed(format!("truncated or corrupt WAV data: {e}"));
    match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Float, 32) => {
            for s in reader.samples::<f32>() {
                interleaved.push(s.map_err(bad)?);
            }
        }
        (SampleFormat::Int, bits @ (8 | 16 | 24 | 32)) => {
            let scale = 1.0 / 2f64.powi(i32::from(bits) - 1);
            for s in reader.samples::<i32>() {
                interleaved.push((f64::from(s.map_err(bad)?) * scale) as f32);
            }
        }
        (format, bits) => {
            return Err(DecodeError::Unsupported(format!(
                "WAV sample format {format:?}/{bits}-bit"
            )))
        }
    }
    finish(interleaved, spec.sample_rate, spec.channels)
}

/// Decode headerless PCM whose layout the caller declared.
///
/// # Errors
///
/// See [`DecodeError`]. Trailing bytes that do not fill a whole frame are
/// rejected as [`DecodeError::Malformed`] rather than silently dropped: a
/// truncated upload should be reported, not quietly shortened.
pub fn decode_raw(
    bytes: &[u8],
    encoding: RawEncoding,
    rate_hz: Option<u32>,
    channels: Option<u16>,
    max_ms: Option<u64>,
) -> Result<Decoded, DecodeError> {
    let rate_hz = rate_hz.ok_or(DecodeError::MissingParameter("sample_rate_hz"))?;
    let channels = channels.ok_or(DecodeError::MissingParameter("channels"))?;
    check_layout(rate_hz, channels)?;
    let frame_bytes = encoding.bytes_per_sample() * usize::from(channels);
    if !bytes.len().is_multiple_of(frame_bytes) {
        return Err(DecodeError::Malformed(format!(
            "{} bytes is not a whole number of {frame_bytes}-byte frames",
            bytes.len()
        )));
    }
    check_duration((bytes.len() / frame_bytes) as u64, rate_hz, max_ms)?;

    let interleaved: Vec<f32> = match encoding {
        RawEncoding::F32Le => bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        RawEncoding::S16Le => bytes
            .chunks_exact(2)
            .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32_768.0)
            .collect(),
    };
    finish(interleaved, rate_hz, channels)
}

fn finish(interleaved: Vec<f32>, rate_hz: u32, channels: u16) -> Result<Decoded, DecodeError> {
    if interleaved.iter().any(|s| !s.is_finite()) {
        return Err(DecodeError::Malformed(
            "audio contains NaN or infinite samples".into(),
        ));
    }
    let mono = downmix(&interleaved, usize::from(channels));
    let samples = resample_mono(&mono, rate_hz, TARGET_SAMPLE_RATE_HZ);
    Ok(Decoded {
        samples,
        source_rate_hz: rate_hz,
        source_channels: channels,
    })
}

/// Average interleaved channels down to mono.
#[must_use]
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let inv = 1.0 / channels as f32;
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() * inv)
        .collect()
}

/// Modified Bessel function of the first kind, order 0 (series expansion),
/// used by the Kaiser window.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / f64::from(k * k);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Band-limited sample-rate conversion of a mono signal.
///
/// Passes the input through untouched when the rates already match. Output
/// length is `floor(len * to / from)`; the kernel is centred (zero group
/// delay), and the signal is treated as zero outside its ends.
///
/// # Panics
///
/// If either rate is zero.
#[must_use]
pub fn resample_mono(input: &[f32], from_hz: u32, to_hz: u32) -> Vec<f32> {
    assert!(from_hz > 0 && to_hz > 0, "sample rates must be non-zero");
    if from_hz == to_hz || input.is_empty() {
        return input.to_vec();
    }
    let g = gcd(u64::from(from_hz), u64::from(to_hz));
    let step = u64::from(from_hz) / g; // input samples consumed per `phases` outputs
    let phases = u64::from(to_hz) / g;

    // Cutoff in cycles per input sample. Downsampling must low-pass at the
    // *output* Nyquist; upsampling only needs to remove images at the input's.
    let ratio = f64::from(to_hz) / f64::from(from_hz);
    let cutoff = 0.5 * ratio.min(1.0) * CUTOFF_FRACTION;
    let half = (KERNEL_ZEROS_PER_SIDE / (2.0 * cutoff)).ceil() as i64;
    let taps = (2 * half + 1) as usize;
    let i0_beta = bessel_i0(KAISER_BETA);

    let out_len = (input.len() as u128 * u128::from(to_hz) / u128::from(from_hz)) as usize;
    let mut out = Vec::with_capacity(out_len);
    let mut tables: Vec<Option<Box<[f32]>>> = vec![None; phases as usize];

    for n in 0..out_len as u64 {
        let pos = n * step;
        let base = (pos / phases) as i64;
        let phase = (pos % phases) as usize;
        let weights = tables[phase].get_or_insert_with(|| {
            let frac = phase as f64 / phases as f64;
            let mut w: Vec<f64> = (0..taps as i64)
                .map(|t| {
                    let k = t - half;
                    let x = k as f64 - frac; // tap offset from the exact output position
                    let arg = 2.0 * cutoff * x;
                    let sinc = if arg.abs() < 1e-12 {
                        1.0
                    } else {
                        (std::f64::consts::PI * arg).sin() / (std::f64::consts::PI * arg)
                    };
                    let r = x / half as f64;
                    let window = if r.abs() >= 1.0 {
                        0.0
                    } else {
                        bessel_i0(KAISER_BETA * (1.0 - r * r).sqrt()) / i0_beta
                    };
                    2.0 * cutoff * sinc * window
                })
                .collect();
            // Unity DC gain in every phase, so a constant stays constant.
            let sum: f64 = w.iter().sum();
            for v in &mut w {
                *v /= sum;
            }
            w.into_iter().map(|v| v as f32).collect()
        });
        let mut acc = 0.0f32;
        for (t, w) in weights.iter().enumerate() {
            let idx = base + t as i64 - half;
            if idx >= 0 && (idx as usize) < input.len() {
                acc += input[idx as usize] * w;
            }
        }
        out.push(acc);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, secs: f64) -> Vec<f32> {
        (0..(f64::from(rate) * secs) as usize)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// RMS of the steady-state middle, ignoring filter edge effects.
    fn middle_rms(x: &[f32]) -> f64 {
        rms(&x[x.len() / 4..3 * x.len() / 4])
    }

    fn encode_wav(spec: hound::WavSpec, frames: &[Vec<f32>]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
            for frame in frames {
                for &s in frame {
                    match (spec.sample_format, spec.bits_per_sample) {
                        (SampleFormat::Float, 32) => w.write_sample(s).unwrap(),
                        (SampleFormat::Int, 16) => w.write_sample((s * 32_767.0) as i16).unwrap(),
                        (SampleFormat::Int, 24) => {
                            w.write_sample((s * 8_388_607.0) as i32).unwrap();
                        }
                        (SampleFormat::Int, 8) => w.write_sample((s * 127.0) as i8).unwrap(),
                        _ => unreachable!(),
                    }
                }
            }
            w.finalize().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn matching_rate_is_a_bit_exact_passthrough() {
        let x = sine(440.0, 16_000, 0.1);
        assert_eq!(resample_mono(&x, 16_000, 16_000), x);
    }

    #[test]
    fn a_passband_tone_keeps_its_amplitude_through_every_common_rate() {
        for rate in [
            8_000, 11_025, 22_050, 24_000, 32_000, 44_100, 48_000, 96_000,
        ] {
            let out = resample_mono(&sine(1_000.0, rate, 1.0), rate, 16_000);
            assert_eq!(out.len(), 16_000, "{rate}");
            let gain = middle_rms(&out) / (1.0 / 2f64.sqrt());
            assert!(
                (gain - 1.0).abs() < 0.01,
                "1 kHz tone from {rate} Hz changed by {gain:.4}×"
            );
        }
    }

    /// The reason this resampler exists: 12 kHz sits above the 16 kHz output's
    /// Nyquist (8 kHz). Naive decimation would fold it to 4 kHz at full
    /// amplitude; a band-limited one removes it.
    #[test]
    fn an_out_of_band_tone_is_removed_not_aliased() {
        let out = resample_mono(&sine(12_000.0, 48_000, 1.0), 48_000, 16_000);
        let residual_db = 20.0 * (middle_rms(&out) / (1.0 / 2f64.sqrt())).log10();
        assert!(
            residual_db < -60.0,
            "12 kHz leaked into the output at {residual_db:.1} dB"
        );

        // Contrast: what naive decimation does to the same input.
        let naive: Vec<f32> = sine(12_000.0, 48_000, 1.0).into_iter().step_by(3).collect();
        let naive_db = 20.0 * (middle_rms(&naive) / (1.0 / 2f64.sqrt())).log10();
        assert!(naive_db > -1.0, "sanity: decimation aliases at full level");
    }

    #[test]
    fn tones_just_below_the_output_nyquist_survive() {
        // 7 kHz is speech-band content (sibilants); the transition band must
        // not eat it.
        let out = resample_mono(&sine(7_000.0, 48_000, 1.0), 48_000, 16_000);
        let gain = middle_rms(&out) / (1.0 / 2f64.sqrt());
        assert!(gain > 0.95, "7 kHz attenuated to {gain:.3}×");
    }

    #[test]
    fn energy_that_would_alias_into_the_top_of_the_band_is_rejected() {
        // 8.8 kHz would fold to 7.2 kHz — inside the region the previous test
        // insists is preserved — so the transition band has to be steep enough
        // to separate the two.
        let out = resample_mono(&sine(8_800.0, 48_000, 1.0), 48_000, 16_000);
        let residual_db = 20.0 * (middle_rms(&out) / (1.0 / 2f64.sqrt())).log10();
        assert!(residual_db < -40.0, "8.8 kHz leaked at {residual_db:.1} dB");
    }

    #[test]
    fn upsampling_preserves_a_tone() {
        let out = resample_mono(&sine(1_000.0, 8_000, 1.0), 8_000, 16_000);
        assert_eq!(out.len(), 16_000);
        let gain = middle_rms(&out) / (1.0 / 2f64.sqrt());
        assert!((gain - 1.0).abs() < 0.01, "{gain}");
    }

    #[test]
    fn a_constant_signal_stays_constant() {
        let out = resample_mono(&vec![0.5; 4_800], 48_000, 16_000);
        for v in &out[100..out.len() - 100] {
            assert!((v - 0.5).abs() < 1e-4, "{v}");
        }
    }

    #[test]
    fn output_length_follows_the_rate_ratio() {
        assert_eq!(
            resample_mono(&vec![0.0; 44_100], 44_100, 16_000).len(),
            16_000
        );
        assert_eq!(
            resample_mono(&vec![0.0; 22_050], 22_050, 16_000).len(),
            16_000
        );
        assert!(resample_mono(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn stereo_is_averaged_to_mono() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[0.2, 0.4], 1), vec![0.2, 0.4]);
    }

    #[test]
    fn a_stereo_48k_wav_becomes_16k_mono() {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let tone = sine(1_000.0, 48_000, 1.0);
        let frames: Vec<Vec<f32>> = tone.iter().map(|s| vec![*s, *s]).collect();
        let d = decode_wav(&encode_wav(spec, &frames), None).unwrap();
        assert_eq!((d.source_rate_hz, d.source_channels), (48_000, 2));
        assert_eq!(d.samples.len(), 16_000);
        assert!((middle_rms(&d.samples) - 1.0 / 2f64.sqrt()).abs() < 0.02);
    }

    #[test]
    fn pcm16_pcm24_pcm8_and_float_wavs_all_decode_to_the_same_level() {
        let tone = sine(500.0, 16_000, 0.5);
        let frames: Vec<Vec<f32>> = tone.iter().map(|s| vec![*s]).collect();
        for (format, bits) in [
            (SampleFormat::Int, 8),
            (SampleFormat::Int, 16),
            (SampleFormat::Int, 24),
            (SampleFormat::Float, 32),
        ] {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: bits,
                sample_format: format,
            };
            let d = decode_wav(&encode_wav(spec, &frames), None).unwrap();
            assert_eq!(d.samples.len(), tone.len(), "{bits}-bit {format:?}");
            let tolerance = if bits == 8 { 0.03 } else { 0.005 };
            assert!(
                (middle_rms(&d.samples) - 1.0 / 2f64.sqrt()).abs() < tolerance,
                "{bits}-bit {format:?} decoded at the wrong level"
            );
        }
    }

    #[test]
    fn garbage_is_malformed_not_a_panic() {
        assert!(matches!(
            decode_wav(b"definitely not a wav file", None),
            Err(DecodeError::Malformed(_))
        ));
        assert!(matches!(
            decode_wav(&[], None),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn a_truncated_wav_is_reported_as_malformed() {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let frames: Vec<Vec<f32>> = sine(300.0, 16_000, 1.0).iter().map(|s| vec![*s]).collect();
        let mut bytes = encode_wav(spec, &frames);
        bytes.truncate(bytes.len() / 2);
        assert!(matches!(
            decode_wav(&bytes, None),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn duration_limit_is_enforced_before_decoding() {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let frames: Vec<Vec<f32>> = vec![vec![0.0]; 32_000]; // 2 s
        let bytes = encode_wav(spec, &frames);
        match decode_wav(&bytes, Some(1_000)) {
            Err(DecodeError::TooLong {
                duration_ms,
                max_ms,
            }) => assert_eq!((duration_ms, max_ms), (2_000, 1_000)),
            other => panic!("expected TooLong, got {other:?}"),
        }
        assert!(
            decode_wav(&bytes, Some(2_000)).is_ok(),
            "exactly at the limit is allowed"
        );
    }

    #[test]
    fn out_of_range_layouts_are_unsupported() {
        for (rate, channels) in [(1_000, 1), (400_000, 1), (16_000, 0), (16_000, 9)] {
            assert!(
                matches!(
                    decode_raw(
                        &[0; 64],
                        RawEncoding::S16Le,
                        Some(rate),
                        Some(channels),
                        None
                    ),
                    Err(DecodeError::Unsupported(_))
                ),
                "{rate} Hz / {channels} ch"
            );
        }
    }

    #[test]
    fn raw_pcm_needs_its_layout_declared() {
        assert_eq!(
            decode_raw(&[0; 4], RawEncoding::S16Le, None, Some(1), None),
            Err(DecodeError::MissingParameter("sample_rate_hz"))
        );
        assert_eq!(
            decode_raw(&[0; 4], RawEncoding::S16Le, Some(16_000), None, None),
            Err(DecodeError::MissingParameter("channels"))
        );
    }

    #[test]
    fn raw_pcm_with_a_partial_frame_is_rejected() {
        // 3 bytes of s16 mono: one and a half samples.
        assert!(matches!(
            decode_raw(&[0; 3], RawEncoding::S16Le, Some(16_000), Some(1), None),
            Err(DecodeError::Malformed(_))
        ));
        // 6 bytes of s16 stereo: one and a half frames.
        assert!(matches!(
            decode_raw(&[0; 6], RawEncoding::S16Le, Some(16_000), Some(2), None),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn raw_f32_native_passes_through_bit_exactly() {
        let src = sine(440.0, 16_000, 0.05);
        let bytes: Vec<u8> = src.iter().flat_map(|s| s.to_le_bytes()).collect();
        let d = decode_raw(&bytes, RawEncoding::F32Le, Some(16_000), Some(1), None).unwrap();
        assert_eq!(d.samples, src);
    }

    #[test]
    fn raw_s16_scales_to_unit_range() {
        let bytes: Vec<u8> = [i16::MAX, i16::MIN, 0]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let d = decode_raw(&bytes, RawEncoding::S16Le, Some(16_000), Some(1), None).unwrap();
        assert!((d.samples[0] - 0.999_97).abs() < 1e-4);
        assert_eq!(d.samples[1], -1.0);
        assert_eq!(d.samples[2], 0.0);
    }

    #[test]
    fn non_finite_samples_are_rejected() {
        let bytes: Vec<u8> = [0.1f32, f32::NAN]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        assert!(matches!(
            decode_raw(&bytes, RawEncoding::F32Le, Some(16_000), Some(1), None),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn decode_timing_for_a_long_clip_is_interactive() {
        // 60 s of 44.1 kHz stereo: the worst realistic upload must resample in
        // well under a second on the blocking pool (release-mode budget is far
        // tighter; this is a debug-build tripwire).
        let mono = sine(300.0, 44_100, 60.0);
        let started = std::time::Instant::now();
        let out = resample_mono(&mono, 44_100, 16_000);
        assert_eq!(out.len(), 960_000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "resampling took {:?}",
            started.elapsed()
        );
    }
}
