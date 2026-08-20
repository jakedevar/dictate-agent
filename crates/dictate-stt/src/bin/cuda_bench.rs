//! Reproducible on-host Whisper decode benchmark.
//!
//! Input is headerless 16 kHz mono f32-le PCM so fixture conversion is explicit
//! and does not pull an audio parser into the production STT crate.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use dictate_stt::{SttRequest, Transcriber, WhisperConfig};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut model = None;
    let mut raw = None;
    let mut iterations = 12_usize;
    let mut warmup = 1_usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => model = Some(PathBuf::from(args.next().context("--model needs a path")?)),
            "--raw-f32" => raw = Some(PathBuf::from(args.next().context("--raw-f32 needs a path")?)),
            "--iterations" => iterations = args.next().context("--iterations needs a value")?.parse()?,
            "--warmup" => warmup = args.next().context("--warmup needs a value")?.parse()?,
            "-h" | "--help" => {
                println!("usage: cuda_bench --model <gguf> --raw-f32 <16kHz-mono-f32le> [--warmup 1] [--iterations 12]");
                return Ok(());
            }
            other => bail!("unknown argument {other}"),
        }
    }
    let model = model.context("--model is required")?;
    let raw = raw.context("--raw-f32 is required")?;
    if iterations < 2 {
        bail!("--iterations must be at least 2");
    }
    let bytes = std::fs::read(&raw).with_context(|| format!("reading {}", raw.display()))?;
    if bytes.len() % 4 != 0 {
        bail!("raw fixture byte length must be divisible by four");
    }
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("exact 4-byte chunks")))
        .collect();
    let config = WhisperConfig {
        model: "large-v3-turbo".into(),
        model_path: model.display().to_string(),
        device: "cuda".into(),
        language: "en".into(),
        initial_prompt: None,
        ..WhisperConfig::default()
    };
    let recognizer = Transcriber::new(&config);
    for run in 0..warmup {
        let result = recognizer.transcribe(&samples, SttRequest::default()).await?;
        let result = result.context("fixture was classified as no speech")?;
        println!("warmup={run} load_ms={:.3} decode_ms={:.3} total_ms={:.3} text={:?}", result.timings.model_load_ms, result.timings.decode_ms, result.timings.total_ms, result.text);
    }
    let mut decoded = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let result = recognizer.transcribe(&samples, SttRequest::default()).await?;
        let result = result.context("fixture was classified as no speech")?;
        decoded.push(result.timings.decode_ms);
        println!("decode_ms={:.3} total_ms={:.3} text={:?}", result.timings.decode_ms, result.timings.total_ms, result.text);
    }
    decoded.sort_by(f64::total_cmp);
    let percentile = |p: f64| decoded[((decoded.len() - 1) as f64 * p).ceil() as usize];
    println!(
        "SUMMARY backend={} samples={} audio_ms={:.3} n={} p50_decode_ms={:.3} p95_decode_ms={:.3}",
        recognizer.model().backend.unwrap_or_else(|| "unknown".into()),
        samples.len(), samples.len() as f64 / 16_000.0 * 1000.0, iterations,
        percentile(0.50), percentile(0.95)
    );
    Ok(())
}
