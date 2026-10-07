//! `dictate notes` against a stub daemon that records what it was asked.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use dictate_proto::{
    Capabilities, Command, CommandResult, ErrorCode, Message, Note, ProtoError, Route, ServerHello,
    ServerInfo,
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

fn notes() -> Vec<Note> {
    vec![
        Note {
            id: 2,
            ts_ms: 1_760_000_000_000,
            text: "email the landlord\nabout the boiler\x1b[2J".into(),
            word_count: 6,
        },
        Note {
            id: 1,
            ts_ms: 0,
            text: "buy oat milk".into(),
            word_count: 3,
        },
    ]
}

type Seen = Arc<Mutex<Vec<Command>>>;

/// A daemon holding two notes; `supports_notes = false` plays an older daemon.
async fn stub(supports_notes: bool) -> (PathBuf, tokio::task::JoinHandle<()>, Seen) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = socket_root().join(format!("dictate-cli-notes-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("dictated.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut line = String::new();
                while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                    let Ok(Message::Request(req)) = serde_json::from_str::<Message>(&line) else {
                        break;
                    };
                    line.clear();
                    if !matches!(&req.command, Command::Handshake(_)) {
                        log.lock().unwrap().push(req.command.clone());
                    }
                    let result = match &req.command {
                        Command::Handshake(_) => {
                            Ok(CommandResult::Handshake(Box::new(ServerHello {
                                protocol_version: dictate_proto::PROTOCOL_VERSION,
                                supported_versions: vec![dictate_proto::PROTOCOL_VERSION],
                                server: ServerInfo::new("dictated", "0.2.0"),
                                capabilities: Capabilities::local_trusted(),
                            })))
                        }
                        _ if !supports_notes
                            && matches!(
                                &req.command,
                                Command::ListNotes { .. } | Command::DeleteNote { .. }
                            ) =>
                        {
                            Err(ProtoError::unsupported_command(req.command.name()))
                        }
                        Command::ListNotes { query, limit, id } => {
                            let mut all = notes();
                            if let Some(id) = id {
                                all.retain(|n| n.id == *id);
                            }
                            if let Some(q) = query {
                                all.retain(|n| n.text.to_lowercase().contains(&q.to_lowercase()));
                            }
                            all.truncate(limit.unwrap_or(100) as usize);
                            Ok(CommandResult::Notes { notes: all })
                        }
                        Command::DeleteNote { id } if *id == 2 => {
                            Ok(CommandResult::Deleted { id: *id })
                        }
                        Command::DeleteNote { id } => Err(ProtoError::new(
                            ErrorCode::NotFound,
                            format!("no note with id {id}"),
                        )),
                        Command::StartDictation { .. } => Ok(CommandResult::SessionStarted {
                            session_id: dictate_proto::SessionId("stub-1".into()),
                        }),
                        other => Err(ProtoError::unsupported_command(other.name())),
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
    (socket, handle, seen)
}

async fn dictate(socket: &PathBuf, args: &[&str]) -> (i32, String, String) {
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_dictate"))
        .args(args)
        .env("DICTATE_SOCKET", socket)
        // No display: `copy` must say so rather than reach for a real clipboard.
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
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
async fn list_is_the_default_and_renders_one_defused_line_per_note() {
    let (socket, server, seen) = stub(true).await;
    let (code, stdout, _) = dictate(&socket, &["notes"]).await;
    assert_eq!(code, 0);
    assert_eq!(stdout.lines().count(), 2, "{stdout}");
    assert!(stdout.contains("2025-10-09 08:53"), "{stdout}");
    assert!(stdout.contains("buy oat milk"), "{stdout}");
    assert!(
        !stdout.contains('\u{1b}'),
        "terminal escapes must be defused: {stdout:?}"
    );
    assert!(stdout.contains("email the landlord"), "{stdout}");
    assert!(
        matches!(
            seen.lock().unwrap()[0],
            Command::ListNotes {
                query: None,
                limit: None,
                id: None
            }
        ),
        "{:?}",
        seen.lock().unwrap()
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_and_limit_reach_the_daemon() {
    let (socket, server, seen) = stub(true).await;
    let (code, stdout, _) = dictate(&socket, &["notes", "search", "oat", "milk"]).await;
    assert_eq!(code, 0);
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    let (code, _, _) = dictate(&socket, &["notes", "list", "--text", "x", "--limit", "5"]).await;
    assert_eq!(code, 0);
    let seen = seen.lock().unwrap();
    assert!(
        matches!(&seen[0], Command::ListNotes { query: Some(q), .. } if q == "oat milk"),
        "{seen:?}"
    );
    assert!(
        matches!(&seen[1], Command::ListNotes { query: Some(q), limit: Some(5), .. } if q == "x"),
        "{seen:?}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn show_prints_the_whole_note_and_json_is_protocol_json() {
    let (socket, server, _) = stub(true).await;
    let (code, stdout, _) = dictate(&socket, &["notes", "show", "2"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("email the landlord\nabout the boiler"),
        "{stdout:?}"
    );
    assert!(!stdout.contains('\u{1b}'), "{stdout:?}");

    let (code, stdout, _) = dictate(&socket, &["notes", "show", "1", "--json"]).await;
    assert_eq!(code, 0);
    match serde_json::from_str::<CommandResult>(stdout.trim()).unwrap() {
        CommandResult::Notes { notes } => assert_eq!(notes[0].text, "buy oat milk"),
        other => panic!("{}", other.name()),
    }

    let (code, _, stderr) = dictate(&socket, &["notes", "show", "99"]).await;
    assert_eq!(code, 1);
    assert!(stderr.contains("no note number 99"), "{stderr}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rm_deletes_by_id_and_reports_a_missing_one() {
    let (socket, server, seen) = stub(true).await;
    let (code, stdout, _) = dictate(&socket, &["notes", "rm", "2"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("deleted 2"), "{stdout}");
    let (code, _, stderr) = dictate(&socket, &["notes", "rm", "7"]).await;
    assert_eq!(code, 1);
    assert!(stderr.contains("no note with id 7"), "{stderr}");
    assert!(matches!(
        seen.lock().unwrap()[0],
        Command::DeleteNote { id: 2 }
    ));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_without_a_display_explains_instead_of_pretending() {
    let (socket, server, _) = stub(true).await;
    let (code, stdout, stderr) = dictate(&socket, &["notes", "copy", "1"]).await;
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("no display"), "{stderr}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_starts_a_session_forced_to_the_note_route_and_start_route_works_too() {
    let (socket, server, seen) = stub(true).await;
    let (code, stdout, _) = dictate(&socket, &["notes", "new"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("recording"), "{stdout}");
    let (code, _, _) = dictate(&socket, &["start", "--route", "note"]).await;
    assert_eq!(code, 0);
    let (code, _, stderr) = dictate(&socket, &["start", "--route", "notes"]).await;
    assert_ne!(code, 0, "a mistyped route must not be forwarded");
    assert!(stderr.contains("unknown route"), "{stderr}");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for c in seen.iter() {
        match c {
            Command::StartDictation {
                options: Some(o), ..
            } => assert_eq!(o.route, Some(Route::Note)),
            other => panic!("{other:?}"),
        }
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_older_daemon_is_reported_not_mistaken_for_an_empty_scratchpad() {
    let (socket, server, _) = stub(false).await;
    let (code, stdout, stderr) = dictate(&socket, &["notes"]).await;
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("not available"), "{stderr}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_mistakes_exit_nonzero_without_contacting_the_daemon() {
    let (socket, server, seen) = stub(true).await;
    for args in [
        &["notes", "show"][..],
        &["notes", "rm", "abc"][..],
        &["notes", "frobnicate"][..],
    ] {
        let (code, _, stderr) = dictate(&socket, args).await;
        assert_ne!(code, 0, "{args:?}");
        assert!(!stderr.is_empty(), "{args:?}");
    }
    assert!(seen.lock().unwrap().is_empty());
    server.abort();
}
