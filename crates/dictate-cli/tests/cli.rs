//! The `dictate` binary, run for real against a stub daemon.
//!
//! The stub speaks the protocol from `dictate-proto` directly, so this test
//! covers what the unit tests cannot: that the binary connects, handshakes,
//! frames NDJSON correctly, renders a result, and exits with the right code.
//!
//! A stub rather than the real daemon because `dictate-cli` deliberately does
//! not depend on `dictated` — see `client::socket_path`. The daemon's own side
//! of this contract is covered by `dictated/tests/control_plane.rs`.

use std::path::PathBuf;
use std::process::Stdio;

use dictate_proto::{
    Capabilities, Command, CommandResult, DaemonInfo, ErrorCode, Message, ModelStatus,
    ProtoError, ServerHello, ServerInfo, State, Status,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// A daemon stub that answers the handshake and one command.
///
/// Returns the socket path and a handle that keeps it alive.
async fn stub_daemon(state: State) -> (PathBuf, tokio::task::JoinHandle<()>) {
    let (socket, server, _) = stub_daemon_with_command_count(state).await;
    (socket, server)
}

/// Like [`stub_daemon`], but exposes the number of non-handshake requests the
/// client made so a keybinding command cannot silently regress to read-then-act.
async fn stub_daemon_with_command_count(
    state: State,
) -> (
    PathBuf,
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("dictate-cli-it-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("dictated.sock");

    let listener = UnixListener::bind(&socket).unwrap();
    let command_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_command_count = command_count.clone();
    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let state = state.clone();
            let command_count = server_command_count.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut line = String::new();
                while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                    let msg: Message = match serde_json::from_str(&line) {
                        Ok(m) => m,
                        Err(_) => break,
                    };
                    line.clear();
                    let Message::Request(req) = msg else { break };
                    if !matches!(&req.command, Command::Handshake(_)) {
                        command_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }

                    let result = match req.command {
                        Command::Handshake(_) => Ok(CommandResult::Handshake(Box::new(ServerHello {
                            protocol_version: dictate_proto::PROTOCOL_VERSION,
                            supported_versions: vec![dictate_proto::PROTOCOL_VERSION],
                            server: ServerInfo::new("dictated", "0.2.0"),
                            capabilities: Capabilities::local_trusted(),
                        }))),
                        Command::GetStatus => Ok(CommandResult::Status(Box::new(Status {
                            state: state.clone(),
                            session: None,
                            daemon: DaemonInfo {
                                name: "dictated".into(),
                                version: "0.2.0".into(),
                                protocol_version: dictate_proto::PROTOCOL_VERSION,
                                pid: Some(4242),
                                uptime_ms: Some(65_000),
                            },
                            model: Some(ModelStatus {
                                name: "large-v3-turbo".into(),
                                loaded: true,
                                backend: Some("cuda".into()),
                            }),
                            capabilities: Capabilities::local_trusted(),
                        }))),
                        Command::Toggle => match state {
                            State::Idle | State::Done | State::Error | State::Cancelled => {
                                Ok(CommandResult::SessionStarted {
                                    session_id: dictate_proto::SessionId("stub-1".into()),
                                })
                            }
                            State::Recording => Ok(CommandResult::SessionStopped {
                                session_id: dictate_proto::SessionId("stub-1".into()),
                            }),
                            ref state => Err(ProtoError::new(
                                ErrorCode::Busy,
                                format!("session is {}; wait for it to finish or cancel it", state.as_str()),
                            )),
                        },
                        Command::StartDictation { .. } => Ok(CommandResult::SessionStarted {
                            session_id: dictate_proto::SessionId("stub-1".into()),
                        }),
                        Command::Stop => Ok(CommandResult::SessionStopped {
                            session_id: dictate_proto::SessionId("stub-1".into()),
                        }),
                        Command::Cancel => Ok(CommandResult::SessionCancelled {
                            session_id: dictate_proto::SessionId("stub-1".into()),
                        }),
                        other => {
                            Err(ProtoError::unsupported_command(other.name()))
                        }
                    };
                    let out = match result {
                        Ok(result) => Message::ok(req.id, result),
                        Err(error) => Message::err(req.id, error),
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

    (socket, handle, command_count)
}

async fn run_dictate(socket: &PathBuf, args: &[&str]) -> (i32, String, String) {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_renders_the_daemons_answer_and_exits_zero() {
    let (socket, server) = stub_daemon(State::Idle).await;
    let (code, stdout, _) = run_dictate(&socket, &["status"]).await;

    assert_eq!(code, 0);
    assert!(stdout.contains("state    idle"), "{stdout}");
    assert!(stdout.contains("dictated 0.2.0"), "{stdout}");
    assert!(stdout.contains("4242"), "the pid must be shown: {stdout}");
    assert!(
        stdout.contains("large-v3-turbo") && stdout.contains("cuda"),
        "the model and its backend are load-bearing for latency reads: {stdout}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_json_emits_a_parseable_protocol_result() {
    let (socket, server) = stub_daemon(State::Idle).await;
    let (code, stdout, _) = run_dictate(&socket, &["status", "--json"]).await;

    assert_eq!(code, 0);
    let parsed: CommandResult = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--json must emit protocol JSON: {e}\n{stdout}"));
    assert!(matches!(parsed, CommandResult::Status(_)));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toggle_starts_when_the_daemon_is_idle() {
    let (socket, server) = stub_daemon(State::Idle).await;
    let (code, stdout, _) = run_dictate(&socket, &["toggle"]).await;

    assert_eq!(code, 0);
    assert!(stdout.contains("recording"), "{stdout}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toggle_is_one_atomic_protocol_request_after_handshake() {
    let (socket, server, commands) = stub_daemon_with_command_count(State::Idle).await;
    let (code, _, _) = run_dictate(&socket, &["toggle"]).await;

    assert_eq!(code, 0);
    assert_eq!(
        commands.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "toggle must not regain a get_status-then-act round trip"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toggle_stops_when_the_daemon_is_recording() {
    let (socket, server) = stub_daemon(State::Recording).await;
    let (code, stdout, _) = run_dictate(&socket, &["toggle"]).await;

    assert_eq!(code, 0);
    assert!(stdout.contains("stopping"), "{stdout}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toggle_mid_pipeline_refuses_rather_than_queueing_a_second_session() {
    let (socket, server) = stub_daemon(State::Transcribing).await;
    let (code, stdout, stderr) = run_dictate(&socket, &["toggle"]).await;

    assert_eq!(code, 1, "a refused toggle must not report success");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("transcribing"),
        "the user must be told what the daemon is busy with: {stderr}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_reports_the_cancelled_session() {
    let (socket, server) = stub_daemon(State::Recording).await;
    let (code, stdout, _) = run_dictate(&socket, &["cancel"]).await;

    assert_eq!(code, 0);
    assert!(stdout.contains("cancelled"), "{stdout}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_the_daemon_does_not_support_fails_with_a_readable_message() {
    let (socket, server) = stub_daemon(State::Idle).await;
    let (code, _, stderr) = run_dictate(&socket, &["dict"]).await;

    assert_eq!(code, 1);
    assert!(
        stderr.contains("not available in this build"),
        "an unimplemented feature must say so plainly: {stderr}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_daemon_is_reported_with_a_usable_hint() {
    let missing = std::env::temp_dir().join("dictate-cli-nothing-here.sock");
    let _ = std::fs::remove_file(&missing);
    let (code, _, stderr) = run_dictate(&missing, &["status"]).await;

    assert_eq!(code, 1);
    assert!(
        stderr.contains("no daemon listening") && stderr.contains("systemctl"),
        "the error must tell the user what to do next: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_arguments_prints_usage_and_exits_two() {
    let (socket, server) = stub_daemon(State::Idle).await;
    let (code, _, stderr) = run_dictate(&socket, &[]).await;

    assert_eq!(code, 2, "a usage error is distinct from a runtime failure");
    assert!(stderr.contains("USAGE"));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_subcommand_exits_two() {
    let (socket, server) = stub_daemon(State::Idle).await;
    let (code, _, stderr) = run_dictate(&socket, &["frobnicate"]).await;

    assert_eq!(code, 2);
    assert!(stderr.contains("frobnicate"), "{stderr}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn help_and_version_work_without_a_daemon() {
    let missing = std::env::temp_dir().join("dictate-cli-nothing-here-2.sock");
    let (code, stdout, _) = run_dictate(&missing, &["--help"]).await;
    assert_eq!(code, 0, "--help must not require a running daemon");
    assert!(stdout.contains("USAGE"));

    let (code, stdout, _) = run_dictate(&missing, &["--version"]).await;
    assert_eq!(code, 0);
    assert!(stdout.starts_with("dictate "), "{stdout}");
}
