//! `dictate transcribe` and `dictate doctor`, run as real processes against a
//! recording stub daemon: what goes on the wire, what is printed where, and
//! what the exit code says.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use dictate_proto::{
    AudioEncoding, AudioSource, Capabilities, Command, CommandResult, DiagnosticCheck,
    DiagnosticsReport, ErrorCode, Message, ProtoError, ServerHello, ServerInfo, Transcript,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

fn socket_root() -> PathBuf {
    let tmp = std::env::temp_dir();
    if tmp.as_os_str().len() <= 48 {
        tmp
    } else {
        PathBuf::from("/tmp")
    }
}

type Seen = Arc<Mutex<Vec<Command>>>;
type Reply = Arc<dyn Fn(&Command) -> Result<CommandResult, ProtoError> + Send + Sync>;

/// A daemon stub that records every non-handshake command and answers each
/// with `reply`.
async fn stub(reply: Reply) -> (PathBuf, Seen, tokio::task::JoinHandle<()>) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = socket_root().join(format!("dictate-cli-ud-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("dictated.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let seen: Seen = Arc::default();
    let seen_task = seen.clone();
    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let seen = seen_task.clone();
            let reply = reply.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut line = String::new();
                while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                    let Ok(Message::Request(req)) = serde_json::from_str::<Message>(&line) else {
                        break;
                    };
                    line.clear();
                    let result = match &req.command {
                        Command::Handshake(_) => {
                            Ok(CommandResult::Handshake(Box::new(ServerHello {
                                protocol_version: dictate_proto::PROTOCOL_VERSION,
                                supported_versions: vec![dictate_proto::PROTOCOL_VERSION],
                                server: ServerInfo::new("dictated", "0.2.0"),
                                capabilities: Capabilities::local_trusted(),
                            })))
                        }
                        other => {
                            seen.lock().unwrap().push(other.clone());
                            reply(other)
                        }
                    };
                    let out = match result {
                        Ok(r) => Message::ok(req.id, r),
                        Err(e) => Message::err(req.id, e),
                    }
                    .to_ndjson_line()
                    .unwrap();
                    if write.write_all(out.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (socket, seen, handle)
}

async fn run(socket: &PathBuf, args: &[&str]) -> (i32, String, String) {
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_dictate"))
        .args(args)
        .env("DICTATE_SOCKET", socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("running the dictate binary");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = socket_root().join(format!("dictate-cli-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The smallest valid WAV: a RIFF header and nothing worth decoding. The stub
/// never decodes it; the CLI only checks the header.
fn tiny_wav() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&36u32.to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&[16, 0, 0, 0, 1, 0, 1, 0]);
    v.extend_from_slice(&16_000u32.to_le_bytes());
    v.extend_from_slice(&32_000u32.to_le_bytes());
    v.extend_from_slice(&[2, 0, 16, 0]);
    v.extend_from_slice(b"data");
    v.extend_from_slice(&0u32.to_le_bytes());
    v
}

fn transcript_reply(text: &str) -> Reply {
    let text = text.to_string();
    Arc::new(move |c| match c {
        Command::TranscribeAudio { .. } => Ok(CommandResult::Transcript(Box::new(
            Transcript::delivered(text.clone()),
        ))),
        other => Err(ProtoError::unsupported_command(other.name())),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcribe_uploads_the_file_and_prints_only_the_text_on_stdout() {
    let (socket, seen, server) = stub(transcript_reply("hello from the daemon")).await;
    let file = temp_file("clip.wav", &tiny_wav());
    let (code, stdout, stderr) = run(&socket, &["transcribe", file.to_str().unwrap()]).await;

    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout, "hello from the daemon\n",
        "stdout is the text, alone, for pipelines"
    );
    assert!(
        stderr.contains("route type"),
        "the summary goes to stderr: {stderr}"
    );

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "one request: {seen:?}");
    match &seen[0] {
        Command::TranscribeAudio {
            audio: AudioSource::Inline { format, data },
            options,
        } => {
            assert_eq!(format.encoding, AudioEncoding::Wav);
            assert_eq!(data, &tiny_wav(), "the file's bytes, untouched");
            let o = options.as_ref().unwrap();
            assert_eq!(o.inject, Some(false), "explicitly not injecting by default");
        }
        other => panic!("{other:?}"),
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_flags_reach_the_daemon_as_session_options() {
    let (socket, seen, server) = stub(transcript_reply("x")).await;
    let file = temp_file("flags.wav", &tiny_wav());
    let (code, ..) = run(
        &socket,
        &[
            "transcribe",
            file.to_str().unwrap(),
            "--inject",
            "--privacy",
            "--route",
            "type",
        ],
    )
    .await;
    assert_eq!(code, 0);
    match &seen.lock().unwrap()[0] {
        Command::TranscribeAudio { options, .. } => {
            let o = options.as_ref().unwrap();
            assert_eq!(o.inject, Some(true));
            assert_eq!(o.privacy, Some(true));
            assert_eq!(o.route, Some(dictate_proto::Route::Type));
        }
        other => panic!("{other:?}"),
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_prints_the_raw_protocol_result() {
    let (socket, _, server) = stub(transcript_reply("json please")).await;
    let file = temp_file("json.wav", &tiny_wav());
    let (code, stdout, _) = run(&socket, &["transcribe", file.to_str().unwrap(), "--json"]).await;
    assert_eq!(code, 0);
    match serde_json::from_str::<CommandResult>(stdout.trim()).unwrap() {
        CommandResult::Transcript(t) => assert_eq!(t.text.as_str(), "json please"),
        other => panic!("{other:?}"),
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_refusal_is_printed_and_fails_the_command() {
    let (socket, _, server) = stub(Arc::new(|_| {
        Err(
            ProtoError::new(ErrorCode::PayloadTooLarge, "the clip is too long")
                .with_detail(serde_json::json!({"limit_ms": 1000, "actual_ms": 2000})),
        )
    }))
    .await;
    let file = temp_file("long.wav", &tiny_wav());
    let (code, stdout, stderr) = run(&socket, &["transcribe", file.to_str().unwrap()]).await;
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("too long") && stderr.contains("payload_too_large"),
        "{stderr}"
    );
    assert!(
        stderr.contains("limit_ms"),
        "the numbers must reach the user: {stderr}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_that_is_not_a_wav_never_reaches_the_daemon() {
    let (socket, seen, server) = stub(transcript_reply("x")).await;
    let file = temp_file("song.mp3", b"ID3\x04\x00 definitely an mp3");
    let (code, _, stderr) = run(&socket, &["transcribe", file.to_str().unwrap()]).await;
    assert_eq!(code, 1);
    assert!(
        stderr.contains("not a WAV") && stderr.contains("ffmpeg"),
        "{stderr}"
    );
    assert!(seen.lock().unwrap().is_empty(), "nothing was sent");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_file_and_a_missing_argument_are_clear() {
    let (socket, _, server) = stub(transcript_reply("x")).await;
    let (code, _, stderr) = run(&socket, &["transcribe", "/no/such/file.wav"]).await;
    assert_eq!(code, 1);
    assert!(stderr.contains("/no/such/file.wav"), "{stderr}");

    let (code, _, stderr) = run(&socket, &["transcribe"]).await;
    assert_eq!(code, 2, "a usage error");
    assert!(stderr.contains("needs a WAV file"), "{stderr}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mistyped_route_is_rejected_before_anything_is_sent() {
    let (socket, seen, server) = stub(transcript_reply("x")).await;
    let file = temp_file("route.wav", &tiny_wav());
    let (code, _, stderr) = run(
        &socket,
        &["transcribe", file.to_str().unwrap(), "--route", "tpye"],
    )
    .await;
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown route 'tpye'"), "{stderr}");
    assert!(seen.lock().unwrap().is_empty());
    server.abort();
}

fn diagnostics_reply(report: DiagnosticsReport) -> Reply {
    Arc::new(move |c| match c {
        Command::Diagnose { .. } => Ok(CommandResult::Diagnostics(Box::new(report.clone()))),
        Command::GetStatus => Err(ProtoError::unsupported_command("get_status")),
        other => Err(ProtoError::unsupported_command(other.name())),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_exits_zero_when_healthy_and_prints_the_daemon_first() {
    let (socket, _, server) = stub(diagnostics_reply(DiagnosticsReport {
        checks: vec![
            DiagnosticCheck::ok("stt_model", "Speech model", "verified"),
            DiagnosticCheck::warn(
                "legacy_pid",
                "Legacy PID",
                "held by the old daemon",
                "stop it",
            ),
        ],
    }))
    .await;
    let (code, stdout, stderr) = run(&socket, &["doctor"]).await;
    assert_eq!(code, 0, "warnings alone do not fail: {stderr}");
    let daemon = stdout.find("Daemon").expect("the daemon line comes first");
    let model = stdout.find("Speech model").unwrap();
    assert!(daemon < model, "{stdout}");
    assert!(stdout.contains("fix: stop it"), "{stdout}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_exits_one_on_a_failure_and_prints_the_fix() {
    let (socket, seen, server) = stub(diagnostics_reply(DiagnosticsReport {
        checks: vec![DiagnosticCheck::fail(
            "grammar_model",
            "Formatter model",
            "model 'qwen3:14b' is not installed",
            "`ollama pull qwen3:14b`, or set grammar.model to one of: gemma4:12b",
        )],
    }))
    .await;
    let (code, stdout, _) = run(&socket, &["doctor", "--quick"]).await;
    assert_eq!(code, 1);
    assert!(
        stdout.contains("[ FAIL ]") && stdout.contains("gemma4:12b"),
        "{stdout}"
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|c| matches!(c, Command::Diagnose { quick: true })),
        "--quick must reach the daemon"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_json_is_the_report() {
    let (socket, _, server) = stub(diagnostics_reply(DiagnosticsReport {
        checks: vec![DiagnosticCheck::ok("a", "A", "fine")],
    }))
    .await;
    let (code, stdout, _) = run(&socket, &["doctor", "--json"]).await;
    assert_eq!(code, 0);
    let report: DiagnosticsReport = serde_json::from_str(stdout.trim()).unwrap();
    assert!(report.check("daemon").is_some() && report.check("a").is_some());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_with_no_daemon_reports_that_first_and_still_checks_what_it_can() {
    let missing = socket_root().join("dictate-cli-doctor-nothing.sock");
    let _ = std::fs::remove_file(&missing);
    let (code, stdout, _) = run(&missing, &["doctor"]).await;
    assert_eq!(code, 1);
    assert!(
        stdout.contains("[ FAIL ]") && stdout.contains("no daemon listening"),
        "{stdout}"
    );
    assert!(
        stdout.contains("systemctl --user start dictated"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Speech model"),
        "the local check still runs: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_against_an_older_daemon_says_so_instead_of_claiming_health() {
    let (socket, _, server) =
        stub(Arc::new(|c| Err(ProtoError::unsupported_command(c.name())))).await;
    let (code, _, stderr) = run(&socket, &["doctor"]).await;
    assert_eq!(code, 1);
    assert!(stderr.contains("cannot run diagnostics"), "{stderr}");
    server.abort();
}
