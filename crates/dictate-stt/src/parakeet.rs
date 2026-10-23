//! A resident, serialized helper. The worker owns framing and the child, so
//! dropping a caller never interrupts a frame or leaves a stale response behind.
use crate::{BoxFuture, ModelInfo, SttProvider, SttRequest, SttTimings, Transcription};
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::Deserialize;
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot, watch},
    time::timeout,
};

const MAX_SAMPLES: usize = 600 * 16_000;
const MAX_LINE: u64 = 64 * 1024;
const LOAD_TIMEOUT: Duration = Duration::from_secs(120);

type Reply = oneshot::Sender<Result<Option<Transcription>>>;
struct Work {
    samples: Vec<f32>,
    reply: Reply,
    queued: Instant,
}
struct Helper {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

/// Kept private to this module except for production construction on macOS.
pub struct ParakeetStt {
    tx: mpsc::Sender<Work>,
    model: Arc<Mutex<ModelInfo>>,
    startup: watch::Receiver<Option<std::result::Result<(), String>>>,
    worker: tokio::task::JoinHandle<()>,
}

impl ParakeetStt {
    pub fn new(config: &crate::config::SttConfig) -> Self {
        Self::start(
            crate::config::expand_tilde(&config.helper_path),
            crate::config::expand_tilde(&config.model_dir),
            None,
        )
    }

    fn start(path: PathBuf, models: PathBuf, test_timeout: Option<Duration>) -> Self {
        let (tx, mut rx) = mpsc::channel::<Work>(1);
        let (startup_tx, startup) = watch::channel(None);
        let model = Arc::new(Mutex::new(ModelInfo {
            name: "parakeet-tdt-0.6b-v2".into(),
            loaded: false,
            backend: None,
        }));
        let status = model.clone();
        let worker = tokio::spawn(async move {
            let mut helper = Helper::spawn(&path, &models).await;
            set_loaded(&status, helper.is_ok());
            let _ = startup_tx.send(Some(
                helper
                    .as_ref()
                    .map(|_| ())
                    .map_err(|e| format!("Parakeet helper preload failed: {e:#}")),
            ));
            let mut failures = 0u32;
            let mut id = 0u64;
            while let Some(work) = rx.recv().await {
                if work.reply.is_closed() {
                    continue;
                }
                let request_started = Instant::now();
                let production_budget =
                    Duration::from_secs_f64(work.samples.len() as f64 / 16000.0 * 3.0 + 5.0);
                if helper.is_err() {
                    // One recovery attempt per request, at most 5 s backoff.
                    // No unbounded retry loop and no replay of failed audio.
                    tokio::time::sleep(Duration::from_millis(
                        (250u64 << failures.min(5)).min(5000),
                    ))
                    .await;
                    if work.reply.is_closed() {
                        continue;
                    }
                    helper = timeout(
                        production_budget.saturating_sub(request_started.elapsed()),
                        Helper::spawn(&path, &models),
                    )
                    .await
                    .unwrap_or_else(|_| Err(anyhow!("Parakeet restart timed out")));
                    set_loaded(&status, helper.is_ok());
                }
                let result = match helper.as_mut() {
                    Ok(helper) => {
                        id += 1;
                        let budget = test_timeout.unwrap_or_else(|| {
                            production_budget.saturating_sub(request_started.elapsed())
                        });
                        let clock = Instant::now();
                        let queue_ms = work.queued.elapsed().as_secs_f64() * 1000.0;
                        match timeout(budget, helper.transcribe(id, &work.samples)).await {
                            Ok(Ok(response)) => Ok(if response.text.trim().is_empty() {
                                None
                            } else {
                                Some(Transcription {
                                    text: response.text,
                                    language: Some("en".into()),
                                    timings: SttTimings {
                                        audio_ms: work.samples.len() as f64 / 16.0,
                                        queue_ms,
                                        model_load_ms: 0.0,
                                        decode_ms: response.ms,
                                        total_ms: queue_ms + clock.elapsed().as_secs_f64() * 1000.0,
                                    },
                                })
                            }),
                            Ok(Err(_)) => Err(anyhow!(
                                "Parakeet helper failed or returned an invalid response"
                            )),
                            Err(_) => Err(anyhow!("Parakeet helper transcription timed out")),
                        }
                    }
                    Err(_) => Err(anyhow!("Parakeet helper restart failed")),
                };
                if result.is_err() {
                    set_loaded(&status, false);
                    if let Ok(mut child) = helper {
                        let _ = child.child.kill().await;
                    }
                    helper = Err(anyhow!("helper requires restart"));
                    failures = failures.saturating_add(1);
                } else {
                    failures = 0;
                }
                // A closed receiver means cancellation: drain without delivering.
                let _ = work.reply.send(result);
            }
            if let Ok(mut helper) = helper {
                let _ = helper.child.kill().await;
            }
        });
        Self {
            tx,
            model,
            startup,
            worker,
        }
    }
}

fn set_loaded(model: &Mutex<ModelInfo>, loaded: bool) {
    let mut model = model.lock().unwrap();
    model.loaded = loaded;
    model.backend = loaded.then(|| "coreml".into());
}

impl Drop for ParakeetStt {
    fn drop(&mut self) {
        self.worker.abort();
    } // Child::kill_on_drop also covers abort/shutdown.
}

impl SttProvider for ParakeetStt {
    fn ready(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let mut startup = self.startup.clone();
            loop {
                if let Some(result) = startup.borrow().clone() {
                    return result.map_err(|e| anyhow!(e));
                }
                startup
                    .changed()
                    .await
                    .context("Parakeet preload worker stopped")?;
            }
        })
    }
    fn model(&self) -> ModelInfo {
        self.model.lock().unwrap().clone()
    }
    fn transcribe<'a>(
        &'a self,
        samples: &'a [f32],
        request: SttRequest,
    ) -> BoxFuture<'a, Result<Option<Transcription>>> {
        Box::pin(async move {
            ensure!(
                samples.len() <= MAX_SAMPLES,
                "Parakeet audio exceeds ten minutes"
            );
            ensure!(
                samples.iter().all(|s| s.is_finite()),
                "Parakeet audio contains non-finite samples"
            );
            ensure!(
                request.language.as_deref().is_none_or(|l| l == "en"),
                "Parakeet v2 supports English only; select Whisper for other languages"
            );
            if samples.is_empty() {
                return Ok(None);
            }
            let (reply, result) = oneshot::channel();
            self.tx
                .try_send(Work {
                    samples: samples.to_vec(),
                    reply,
                    queued: Instant::now(),
                })
                .map_err(|_| anyhow!("Parakeet worker busy or stopped"))?;
            result.await.context("Parakeet worker stopped")?
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResponse {
    id: u64,
    text: Option<String>,
    ms: Option<f64>,
    error: Option<String>,
}
struct Response {
    text: String,
    ms: f64,
}

impl Helper {
    async fn spawn(path: &PathBuf, models: &PathBuf) -> Result<Self> {
        let mut child = Command::new(path)
            .arg("--models")
            .arg(models)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawning Parakeet helper")?;
        let mut helper = Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
        };
        let ready = timeout(LOAD_TIMEOUT, helper.line())
            .await
            .context("Parakeet preload timed out")??;
        ensure!(
            ready == b"{\"ready\":true}\n",
            "invalid Parakeet readiness response"
        );
        Ok(helper)
    }
    async fn line(&mut self) -> Result<Vec<u8>> {
        let mut line = Vec::new();
        (&mut self.output)
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)
            .await?;
        ensure!(
            line.len() as u64 <= MAX_LINE && line.last() == Some(&b'\n'),
            "invalid helper response framing"
        );
        Ok(line)
    }
    async fn transcribe(&mut self, id: u64, samples: &[f32]) -> Result<Response> {
        let header = format!("{{\"id\":{id},\"sample_rate\":16000}}");
        let size = 4 + header.len() + samples.len() * 4;
        self.input.write_all(&(size as u32).to_le_bytes()).await?;
        self.input
            .write_all(&(header.len() as u32).to_le_bytes())
            .await?;
        self.input.write_all(header.as_bytes()).await?;
        let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.input.write_all(&pcm).await?;
        self.input.flush().await?;
        let response: WireResponse = serde_json::from_slice(&self.line().await?)?;
        ensure!(response.id == id, "invalid helper response");
        if response.error.is_some() {
            bail!("Parakeet decode failed");
        }
        match (response.text, response.ms) {
            (Some(text), Some(ms)) if ms.is_finite() && ms >= 0.0 => Ok(Response { text, ms }),
            _ => bail!("missing helper result"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        path: PathBuf,
    }
    impl Fixture {
        fn new(mode: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "parakeet-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            let script = format!(
                r#"#!/bin/sh
cd "$(dirname "$0")" || exit 1
printf '%s\n' $$ >> pids
printf '{{"ready":true}}\n'
while :; do
    size=$(dd bs=1 count=4 2>/dev/null | od -An -tu4 | tr -d ' ')
    [ -n "$size" ] || exit 0
    header_size=$(dd bs=1 count=4 2>/dev/null | od -An -tu4 | tr -d ' ')
    header=$(dd bs=1 count="$header_size" 2>/dev/null)
    id=$(printf '%s' "$header" | sed 's/.*"id":\([0-9]*\).*/\1/')
    dd bs=1 count=$((size - 4 - header_size)) of=/dev/null 2>/dev/null
    case {mode} in
        crash) if [ ! -f failed ]; then touch failed; exit 1; fi ;;
        timeout) if [ ! -f failed ]; then touch failed; sleep 0.3; fi ;;
        slow) sleep 0.1 ;;
        wrongid) id=999 ;;
        malformed) printf 'garbage\n'; continue ;;
        long) dd if=/dev/zero bs=65537 count=1 2>/dev/null; continue ;;
    esac
    printf '{{"id":%s,"text":"Synthetic result.","ms":1.5}}\n' "$id"
done
"#
            );
            std::fs::write(path.join("helper"), script).unwrap();
            std::fs::set_permissions(path.join("helper"), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            Self { path }
        }
        fn provider(&self, budget: Option<Duration>) -> ParakeetStt {
            ParakeetStt::start(self.path.join("helper"), self.path.clone(), budget)
        }
        fn launches(&self) -> usize {
            std::fs::read_to_string(self.path.join("pids"))
                .unwrap()
                .lines()
                .count()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[tokio::test]
    async fn framed_roundtrip_keeps_resident_helper() {
        let f = Fixture::new("normal");
        let p = f.provider(None);
        p.ready().await.unwrap();
        assert!(p.model().loaded);
        for _ in 0..2 {
            let t = p
                .transcribe(&[0.25, -0.5], SttRequest::default())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(t.text, "Synthetic result.");
            assert_eq!(t.language.as_deref(), Some("en"));
            assert_eq!(t.timings.decode_ms, 1.5);
        }
        assert_eq!(f.launches(), 1);
    }
    #[tokio::test]
    async fn crashes_and_timeouts_restart_without_replaying_audio() {
        for mode in ["crash", "timeout"] {
            let f = Fixture::new(mode);
            let p = f.provider(Some(Duration::from_millis(100)));
            p.ready().await.unwrap();
            assert!(p.transcribe(&[0.0], SttRequest::default()).await.is_err());
            assert!(!p.model().loaded);
            assert!(p
                .transcribe(&[0.0], SttRequest::default())
                .await
                .unwrap()
                .is_some());
            assert_eq!(f.launches(), 2);
        }
    }
    #[tokio::test]
    async fn cancellation_drains_response_without_killing_healthy_helper() {
        let f = Fixture::new("slow");
        let p = f.provider(None);
        p.ready().await.unwrap();
        let samples = [0.0];
        let future = p.transcribe(&samples, SttRequest::default());
        assert!(timeout(Duration::from_millis(40), future).await.is_err());
        assert!(p
            .transcribe(&samples, SttRequest::default())
            .await
            .unwrap()
            .is_some());
        assert_eq!(f.launches(), 1);
    }
    #[tokio::test]
    async fn protocol_errors_mark_unloaded_and_are_bounded() {
        for mode in ["wrongid", "malformed", "long"] {
            let f = Fixture::new(mode);
            let p = f.provider(None);
            p.ready().await.unwrap();
            assert!(p.transcribe(&[0.0], SttRequest::default()).await.is_err());
            assert!(!p.model().loaded);
        }
    }
    #[tokio::test]
    async fn invalid_requests_never_reach_helper() {
        let f = Fixture::new("normal");
        let p = f.provider(None);
        p.ready().await.unwrap();
        assert!(p
            .transcribe(&vec![0.0; MAX_SAMPLES + 1], SttRequest::default())
            .await
            .is_err());
        assert!(p
            .transcribe(&[f32::NAN], SttRequest::default())
            .await
            .is_err());
        assert!(p
            .transcribe(
                &[0.0],
                SttRequest {
                    language: Some("fr".into()),
                    ..Default::default()
                }
            )
            .await
            .is_err());
        assert!(p
            .transcribe(&[], SttRequest::default())
            .await
            .unwrap()
            .is_none());
        assert_eq!(f.launches(), 1);
    }
    #[tokio::test]
    async fn failed_preload_never_reports_ready() {
        let f = Fixture::new("normal");
        std::fs::remove_file(f.path.join("helper")).unwrap();
        let p = f.provider(None);
        assert!(p.ready().await.is_err());
        assert!(!p.model().loaded);
    }
}
