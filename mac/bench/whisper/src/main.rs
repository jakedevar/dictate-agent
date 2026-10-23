use serde_json::json;
use std::{error::Error, fs, path::Path, time::Instant};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

fn peak_rss_bytes() -> u64 {
    // getrusage writes a fully initialized rusage struct on success. There are
    // no borrowed pointers or concurrent writes; RUSAGE_SELF is process-local.
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(result, 0, "getrusage failed");
    let usage = unsafe { usage.assume_init() };
    let bytes = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        bytes
    } else {
        bytes * 1024
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    let manifest = Path::new(&args[1]);
    let base = manifest.parent().ok_or("manifest needs parent")?;
    let mut clips = Vec::new();
    for row in fs::read_to_string(manifest)?.lines() {
        let f: Vec<_> = row.split('\t').map(str::to_owned).collect();
        let mut wav = hound::WavReader::open(base.join(&f[2]))?;
        let spec = wav.spec();
        assert_eq!(
            (spec.sample_rate, spec.channels, spec.bits_per_sample),
            (16000, 1, 16)
        );
        let samples = wav
            .samples::<i16>()
            .map(|s| s.map(|v| f32::from(v) / 32768.0))
            .collect::<Result<Vec<_>, _>>()?;
        clips.push((f, samples));
    }
    let start = Instant::now();
    let mut parameters = WhisperContextParameters::default();
    parameters.use_gpu(cfg!(any(feature = "metal", feature = "cuda")));
    let context = WhisperContext::new_with_params(&args[2], parameters)?;
    let mut state = context.create_state()?;
    let engine = if cfg!(feature = "metal") {
        "whisper-metal"
    } else if cfg!(feature = "cuda") {
        "whisper-cuda"
    } else {
        "whisper-cpu"
    };
    println!(
        "{}",
        json!({"kind":"load", "engine":engine, "ms":start.elapsed().as_secs_f64()*1000.0, "peak_rss_bytes":peak_rss_bytes()})
    );
    for pass in -1..3 {
        for (f, samples) in &clips {
            let start = Instant::now();
            let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            p.set_language(Some("en"));
            p.set_no_context(true);
            p.set_n_threads(6);
            p.set_no_speech_thold(0.6);
            p.set_print_progress(false);
            p.set_print_realtime(false);
            p.set_print_timestamps(false);
            state.full(p, samples)?;
            let mut text = String::new();
            for i in 0..state.full_n_segments() {
                text.push_str(
                    &state
                        .get_segment(i)
                        .ok_or("missing segment")?
                        .to_str_lossy()?,
                );
            }
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            if pass >= 0 {
                println!(
                    "{}",
                    json!({"kind":"clip", "engine":engine, "id":f[0], "set":f[1], "pass":pass, "ms":ms, "audio_s":samples.len() as f64/16000.0, "text":text.trim(), "peak_rss_bytes":peak_rss_bytes()})
                );
            }
        }
    }
    Ok(())
}
