//! Real-hardware end-to-end: does `dictated` actually work on this machine?
//!
//! Everything that *can* be real is: the Whisper model runs on the GPU from the
//! installed `ggml-large-v3-turbo.bin` (opened read-only — never written), Silero
//! VAD runs on the CPU, and the daemon is the real `Daemon` on a real unix
//! socket. What is not real is deliberate: the injector is a mock (nothing is
//! typed anywhere), the microphone does not exist (audio-less mode — no input
//! device is ever opened), and every runtime/data path is a private temp
//! directory, so this can run while Jake's daily-driver daemon is up.
//!
//! Compiled only with `--features e2e-real`; run it with `just e2e`.
//!
//! # What the fixtures are, and are not
//!
//! `tests/fixtures/e2e/*.wav` are espeak-ng speech synthesised from invented
//! sentences (see `scripts/gen-e2e-fixtures.sh`). The voice is robotic and
//! noise-free, so a low word error rate here proves the plumbing and the
//! model's basic health — it does **not** predict accuracy on a real voice in a
//! real room.
//!
//! # Environment knobs
//!
//! - `E2E_RUNS` — measured runs per fixture (default 8, after one warm-up).
//! - `E2E_MODEL` — path to the GGUF (default: the standard install location).
//! - `E2E_WER_MAX` — word-error-rate ceiling (default 0.10).
//! - `E2E_LLM_MODEL` — turn the LLM formatting pass on with this one Ollama
//!   model (default: off, so the run does not depend on installed models).

mod harness;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dictate_core::config::{Config, ConfigReport};
use dictate_core::llm_formatter::LlmFormatterPort;
use dictate_core::ports::mock::{MockInjector, NullEarcons, NullMedia, RecordingNotifier};
use dictate_core::ports::{AudioSource, DisabledAudioSource, WhisperStt};
use dictate_core::Pipeline;
use dictate_history::{HistoryConfig, HistoryStore};
use dictate_proto::{
    AudioFormat, AudioSource as Upload, Command, CommandResult, SessionOptions, StageTiming,
    Transcript,
};
use dictated::paths::RuntimePaths;
use dictated::{Daemon, DaemonExtras};
use harness::Client;

/// Report line. whisper.cpp writes hundreds of lines of its own to the same
/// streams; the `e2e| ` prefix is what `just e2e` filters the summary on.
macro_rules! say {
    ($($arg:tt)*) => {
        println!("e2e| {}", format!($($arg)*))
    };
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/// Lowercase, keep letters/digits/apostrophes, everything else is a separator.
fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Word error rate: word-level edit distance over the reference length.
fn wer(reference: &str, hypothesis: &str) -> f64 {
    let r = normalize(reference);
    let h = normalize(hypothesis);
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, hw) in h.iter().enumerate() {
            let sub = prev[j] + usize::from(rw != hw);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[h.len()] as f64 / r.len() as f64
}

#[test]
fn the_wer_scorer_is_itself_correct() {
    assert_eq!(
        wer("the cat sat", "The cat sat."),
        0.0,
        "case and punctuation are ignored"
    );
    assert!(
        (wer("the cat sat", "the cat") - 1.0 / 3.0).abs() < 1e-9,
        "one deletion"
    );
    assert!(
        (wer("the cat sat", "the dog sat") - 1.0 / 3.0).abs() < 1e-9,
        "one substitution"
    );
    assert!((wer("a b", "a x b") - 0.5).abs() < 1e-9, "one insertion");
    assert_eq!(wer("hello", ""), 1.0);
    assert_eq!(wer("", ""), 0.0);
    assert_eq!(normalize("Don't stop -- go!"), ["don't", "stop", "go"]);
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

struct Series(Vec<f64>);

impl Series {
    fn p50(&self) -> f64 {
        let mut v = self.0.clone();
        v.sort_by(f64::total_cmp);
        percentile(&v, 50.0)
    }
    fn p95(&self) -> f64 {
        let mut v = self.0.clone();
        v.sort_by(f64::total_cmp);
        percentile(&v, 95.0)
    }
}

// ---------------------------------------------------------------------------
// The real daemon
// ---------------------------------------------------------------------------

fn model_path() -> PathBuf {
    std::env::var_os("E2E_MODEL").map_or_else(
        || {
            dictate_stt::config::expand_tilde(
                "~/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin",
            )
        },
        PathBuf::from,
    )
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e")
}

struct Fixture {
    name: &'static str,
    wav: Vec<u8>,
    text: String,
}

fn fixtures() -> Vec<Fixture> {
    ["short", "medium", "long"]
        .into_iter()
        .map(|name| Fixture {
            name,
            wav: std::fs::read(fixtures_dir().join(format!("{name}.wav"))).unwrap_or_else(|e| {
                panic!("fixture {name}.wav: {e} (run scripts/gen-e2e-fixtures.sh)")
            }),
            text: std::fs::read_to_string(fixtures_dir().join(format!("{name}.txt")))
                .expect("fixture transcript"),
        })
        .collect()
}

/// One real model at a time: each test loads its own copy (~1.6 GB of VRAM), and
/// the tests would otherwise run in parallel and fight over the GPU.
static ONE_MODEL_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Real {
    _exclusive: tokio::sync::MutexGuard<'static, ()>,
    daemon: Daemon,
    pipeline: Arc<Pipeline>,
    injector: Arc<MockInjector>,
    dir: PathBuf,
    /// Wall time from starting to build the pipeline until the model was loaded.
    cold_start: Duration,
    /// Just the model load, as reported by the first `loaded` observation.
    model_ready: Duration,
}

impl Real {
    async fn start() -> Self {
        let exclusive = ONE_MODEL_AT_A_TIME.lock().await;
        // This test must never reach a real desktop: not to type (the injector
        // is a mock) and not even to probe. Hide the session from the daemon
        // under test. Tests are serialised by the lock above, and nothing else
        // in this process reads the environment concurrently.
        for var in ["DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"] {
            // SAFETY: see above — single test at a time, before the daemon starts.
            unsafe { std::env::remove_var(var) };
        }
        let model = model_path();
        assert!(
            model.exists(),
            "the speech model is not installed at {} — run `dictate model pull large-v3-turbo` \
             (this test never downloads or writes into the model directory)",
            model.display()
        );

        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Short root: unix socket paths are capped at 108 bytes and the RSI
        // harness exports a long TMPDIR.
        let dir = PathBuf::from("/tmp").join(format!("dictated-e2e-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut config = Config::default();
        config.whisper.model = "large-v3-turbo".into();
        config.whisper.model_path = model.to_string_lossy().into_owned();
        config.whisper.device = "cuda".into();
        config.whisper.language = "en".into();
        config.audio.capture = false;
        config.audio.pre_roll_ms = 0;
        config.notifications.enabled = false;
        // The LLM pass is off unless a run asks for it: this test is about the
        // speech path, and must not depend on which Ollama models are installed.
        config.format.llm.enabled = false;
        if let Ok(m) = std::env::var("E2E_LLM_MODEL") {
            config.format.llm.enabled = true;
            config.format.llm.models = vec![m];
        }

        let started = Instant::now();
        let stt = Arc::new(WhisperStt::new(&config.whisper));
        let injector = Arc::new(MockInjector::new());
        let history = Arc::new(Mutex::new(
            HistoryStore::new(&HistoryConfig {
                enabled: true,
                db_path: dir.join("history.db").to_string_lossy().into_owned(),
                ..HistoryConfig::default()
            })
            .unwrap(),
        ));
        let pipeline = Arc::new(Pipeline {
            // Isolated: never read the real desktop's focus from a test.
            context: Arc::new(dictate_core::ContextEngine::disabled()),
            dictionary: None,
            // The real deterministic chain, exactly as the daemon builds it.
            text_chain: Arc::new(dictate_fmt::TextChain::standard(&config.format)),
            audio: Arc::new(DisabledAudioSource::new(&config.audio)) as Arc<dyn AudioSource>,
            stt: stt.clone(),
            vad: Arc::new(dictate_vad::SileroVad::new(config.vad.clone()).unwrap()),
            formatter: Arc::new(LlmFormatterPort::new(config.format.llm.clone())),
            injector: injector.clone(),
            notifier: Arc::new(RecordingNotifier::default()),
            media: Arc::new(NullMedia),
            earcons: Arc::new(NullEarcons),
            history: history.clone(),
            local: Arc::new(dictate_core::local_executor::LocalExecutor::new(
                &config.local,
            )),
            timer: Arc::new(dictate_core::timer::TimerExecutor::new(&config.timer)),
            local_model: config.local.model.clone(),
        });

        let runtime = RuntimePaths::under(&dir);
        let doctor = Arc::new(dictated::doctor::Doctor::new(
            config,
            ConfigReport::default(),
            runtime.clone(),
            pipeline.clone(),
        ));
        let daemon = Daemon::start_with(
            pipeline.clone(),
            history,
            &runtime,
            dictated::server::local_capabilities(true),
            None,
            DaemonExtras {
                diagnostics: Some(doctor),
            },
        )
        .await
        .expect("daemon starts");
        let daemon_up = started.elapsed();

        // The model loads on its own thread; "ready" means it answered.
        let deadline = Instant::now() + Duration::from_secs(120);
        while !pipeline.stt.model().loaded {
            assert!(
                Instant::now() < deadline,
                "the model did not load within 120 s"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let cold_start = started.elapsed();
        say!(
            "cold start: daemon bound in {:.0} ms, model resident after {:.0} ms (backend: {})",
            daemon_up.as_secs_f64() * 1000.0,
            cold_start.as_secs_f64() * 1000.0,
            pipeline.stt.model().backend.unwrap_or_default()
        );
        Self {
            _exclusive: exclusive,
            daemon,
            pipeline,
            injector,
            dir,
            cold_start,
            model_ready: cold_start.saturating_sub(daemon_up),
        }
    }

    async fn client(&self) -> Client {
        let mut c = Client::connect(self.daemon.socket()).await;
        c.handshake().await;
        c
    }

    async fn stop(self) {
        let dir = self.dir.clone();
        self.daemon.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn transcribe(wav: &[u8]) -> Command {
    Command::TranscribeAudio {
        audio: Upload::Inline {
            format: AudioFormat::wav(),
            data: wav.to_vec(),
        },
        options: Some(SessionOptions {
            privacy: Some(true),
            ..SessionOptions::default()
        }),
    }
}

async fn run_one(client: &mut Client, wav: &[u8]) -> (Transcript, f64) {
    let started = Instant::now();
    let result = client
        .request(transcribe(wav))
        .await
        .expect("transcription succeeds");
    let wall = started.elapsed().as_secs_f64() * 1000.0;
    match result {
        CommandResult::Transcript(t) => (*t, wall),
        other => panic!("expected a transcript, got {other:?}"),
    }
}

fn ms(t: &StageTiming) -> f64 {
    t.elapsed_ms().unwrap_or(f64::NAN)
}

/// GPU memory this process holds, and what the whole GPU has in use, via
/// `nvidia-smi`. `None` when the tool is unavailable.
fn gpu_memory() -> Option<(u64, u64, u64)> {
    let pid = std::process::id().to_string();
    let apps = std::process::Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    let mine = String::from_utf8_lossy(&apps.stdout)
        .lines()
        .find_map(|l| {
            let mut c = l.split(',').map(str::trim);
            (c.next()? == pid).then(|| c.next()?.parse::<u64>().ok())?
        })
        .unwrap_or(0);
    let total = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&total.stdout)
        .lines()
        .next()?
        .to_string();
    let mut c = line.split(',').map(str::trim);
    Some((mine, c.next()?.parse().ok()?, c.next()?.parse().ok()?))
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// The headline check: real speech in, correct text out, on the GPU, with
/// per-stage latency measured over repeated runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_whisper_on_cuda_transcribes_the_fixtures_accurately_and_fast() {
    let runs: usize = std::env::var("E2E_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let wer_max: f64 = std::env::var("E2E_WER_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.10);

    let real = Real::start().await;
    assert_eq!(
        real.pipeline.stt.model().backend.as_deref(),
        Some("cuda"),
        "this test exists to prove the GPU path; a CPU fallback is a failure"
    );
    let mut client = real.client().await;

    say!(
        "{:<8} {:>7} | {:>26} | {:>26} | {:>26} | {:>26} | {:>5}",
        "clip",
        "audio",
        "capture(dec+rs) p50/p95",
        "vad p50/p95",
        "stt p50/p95",
        "total p50/p95",
        "WER"
    );
    for f in fixtures() {
        // One warm-up: the first decode pays CUDA kernel/graph warm-up, which
        // is reported separately so it cannot flatter or poison the steady state.
        let (first, first_wall) = run_one(&mut client, &f.wav).await;
        say!(
            "{:<8} first run: stt {:.0} ms, total {:.0} ms (wall {:.0} ms), WER {:.3}",
            f.name,
            ms(&first.timings.stt),
            first.timings.total_ms.unwrap_or(f64::NAN),
            first_wall,
            wer(&f.text, first.text.as_str())
        );

        let (mut capture, mut vad, mut stt, mut total, mut wall) =
            (vec![], vec![], vec![], vec![], vec![]);
        let mut worst = 0.0f64;
        let mut audio_ms = 0.0;
        for _ in 0..runs {
            let (t, w) = run_one(&mut client, &f.wav).await;
            let e = wer(&f.text, t.text.as_str());
            worst = worst.max(e);
            assert!(
                e <= wer_max,
                "{}: WER {e:.3} exceeds {wer_max}\n  heard:    {}\n  expected: {}",
                f.name,
                t.text.as_str(),
                f.text.trim()
            );
            assert!(matches!(t.timings.stt, StageTiming::Ran { .. }));
            capture.push(ms(&t.timings.capture));
            vad.push(ms(&t.timings.vad));
            stt.push(ms(&t.timings.stt));
            total.push(t.timings.total_ms.unwrap());
            wall.push(w);
            audio_ms = t.timings.audio_ms.unwrap_or(0.0);
        }
        let (capture, vad, stt, total, wall) = (
            Series(capture),
            Series(vad),
            Series(stt),
            Series(total),
            Series(wall),
        );
        say!(
            "{:<8} {:>5.1} s | {:>10.1} / {:<10.1} | {:>10.1} / {:<10.1} | {:>10.1} / {:<10.1} | {:>10.1} / {:<10.1} | {:>5.3}",
            f.name,
            audio_ms / 1000.0,
            capture.p50(),
            capture.p95(),
            vad.p50(),
            vad.p95(),
            stt.p50(),
            stt.p95(),
            total.p50(),
            total.p95(),
            worst
        );
        say!(
            "{:<8}   request wall clock p50/p95: {:.1} / {:.1} ms   real-time factor (stt p50): {:.3}",
            "",
            wall.p50(),
            wall.p95(),
            stt.p50() / audio_ms
        );
    }

    if let Some((mine, used, total)) = gpu_memory() {
        say!(
            "GPU memory: this daemon {mine} MiB; whole GPU {used}/{total} MiB in use \
             (includes every other process)"
        );
        assert!(
            mine > 0,
            "CUDA is selected but this process holds no GPU memory"
        );
    }
    say!(
        "cold start to ready: {:.0} ms (model load {:.0} ms)",
        real.cold_start.as_secs_f64() * 1000.0,
        real.model_ready.as_secs_f64() * 1000.0
    );

    assert!(
        real.injector.injected().is_empty(),
        "the E2E test must never type anything"
    );
    real.stop().await;
}

/// The daemon's own diagnosis agrees with reality: CUDA verified against the
/// driver, the installed model matching the pinned SHA-256.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_real_doctor_verifies_the_model_and_the_gpu() {
    let real = Real::start().await;
    let mut client = real.client().await;
    // Hashing 1.6 GB is seconds in release and much longer in a debug build.
    let report = match client
        .request_within(Duration::from_secs(300), Command::Diagnose { quick: false })
        .await
        .unwrap()
    {
        CommandResult::Diagnostics(r) => *r,
        other => panic!("{other:?}"),
    };
    for c in &report.checks {
        say!(
            "[{:?}] {}: {}{}",
            c.status,
            c.id,
            c.detail,
            c.fix
                .as_deref()
                .map(|f| format!("  (fix: {f})"))
                .unwrap_or_default()
        );
    }
    let model = report.check("stt_model").unwrap();
    assert_eq!(
        model.status,
        dictate_proto::CheckStatus::Ok,
        "{}",
        model.detail
    );
    assert!(model.detail.contains("verified"), "{}", model.detail);
    let backend = report.check("stt_backend").unwrap();
    assert_eq!(
        backend.status,
        dictate_proto::CheckStatus::Ok,
        "{}",
        backend.detail
    );
    assert!(backend.detail.contains("GPU memory"), "{}", backend.detail);
    // Audio-less mode really is audio-less.
    assert!(report
        .check("audio_input")
        .unwrap()
        .detail
        .contains("audio-less"));
    assert!(
        report.is_healthy()
            || report
                .checks
                .iter()
                .all(|c| c.id == "ollama" || c.status != dictate_proto::CheckStatus::Fail),
        "only an unrelated Ollama problem may fail this report"
    );
    real.stop().await;
}

/// Cancelling a long real transcription must not wedge the daemon: the next
/// upload still gets the right answer, and how long it had to wait is reported
/// (Whisper's decode cannot be interrupted mid-flight, so the answer to "how
/// long" is a measurement, not an assumption).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_real_transcription_leaves_the_daemon_usable() {
    let real = Real::start().await;
    let f = fixtures();
    let (short, long) = (&f[0], &f[2]);
    // Warm up so the measurement below is the steady state.
    let mut client = real.client().await;
    let _ = run_one(&mut client, &short.wav).await;

    let mut uploader = real.client().await;
    let long_wav = long.wav.clone();
    let pending = tokio::spawn(async move { uploader.request(transcribe(&long_wav)).await });
    // Let it reach STT, then cancel from a second connection.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut canceller = real.client().await;
    let cancelled = canceller.request(Command::Cancel).await;
    assert!(cancelled.is_ok(), "{cancelled:?}");
    let err = pending.await.unwrap().unwrap_err();
    assert_eq!(err.code, dictate_proto::ErrorCode::Cancelled);

    let started = Instant::now();
    let (t, _) = run_one(&mut client, &short.wav).await;
    let waited = started.elapsed();
    let e = wer(&short.text, t.text.as_str());
    say!(
        "next upload after cancelling a {:.0} s decode: answered in {:.0} ms, WER {e:.3}",
        long.wav.len() as f64 / 32_000.0,
        waited.as_secs_f64() * 1000.0
    );
    assert!(e <= 0.10);
    assert!(
        waited < Duration::from_secs(30),
        "the daemon was wedged for {waited:?}"
    );
    real.stop().await;
}

/// A silent clip never reaches the recognizer (so it cannot hallucinate text).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn silence_is_gated_by_the_real_vad_and_never_transcribed() {
    let real = Real::start().await;
    let mut client = real.client().await;
    let silence: Vec<u8> = {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
            for _ in 0..48_000 {
                w.write_sample(0i16).unwrap();
            }
            w.finalize().unwrap();
        }
        buf.into_inner()
    };
    let (t, _) = run_one(&mut client, &silence).await;
    assert_eq!(t.text.as_str(), "");
    assert!(
        !t.timings.stt.did_run(),
        "no speech means no inference: {:?}",
        t.timings.stt
    );
    real.stop().await;
}
