use std::path::Path;
use std::time::Instant;

use dictate_vad::{GateDecision, SileroVad, VadConfig, VoiceActivityGate};

fn wav(name: &str) -> Vec<f32> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let mut reader = hound::WavReader::open(path).expect("committed golden WAV must open");
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 16_000, "fixtures must be 16 kHz");
    assert_eq!(spec.channels, 1, "fixtures must be mono");
    reader
        .samples::<i16>()
        .map(|sample| sample.expect("valid PCM") as f32 / i16::MAX as f32)
        .collect()
}

fn vad() -> SileroVad {
    SileroVad::new(VadConfig::default()).unwrap()
}

#[test]
fn speech_golden_wav_is_retained_and_reports_measurement() {
    let samples = wav("speech.wav");
    let started = Instant::now();
    let decision = vad().gate(&samples).unwrap();
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(elapsed_ms.is_finite() && elapsed_ms >= 0.0);
    match decision {
        GateDecision::Speech {
            samples: trimmed,
            leading_trimmed_ms,
            trailing_trimmed_ms,
        } => {
            assert!(!trimmed.is_empty());
            assert!(trimmed.len() <= samples.len());
            eprintln!(
                "S11 fixture latency: VAD={elapsed_ms:.2}ms, STT input={:.1}ms→{:.1}ms, trim={:.1}ms",
                samples.len() as f64 / 16.0,
                trimmed.len() as f64 / 16.0,
                leading_trimmed_ms + trailing_trimmed_ms,
            );
        }
        GateDecision::NoSpeech => panic!("speech golden WAV was rejected by Silero"),
    }
}

#[test]
fn silence_golden_wav_skips_stt() {
    assert_eq!(
        vad().gate(&wav("silence.wav")).unwrap(),
        GateDecision::NoSpeech
    );
}

#[test]
fn quiet_golden_wav_skips_stt() {
    assert_eq!(
        vad().gate(&wav("quiet.wav")).unwrap(),
        GateDecision::NoSpeech
    );
}
