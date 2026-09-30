//! A stub `dictated`: a real unix socket speaking the real NDJSON envelope,
//! with scripted answers. Shared by the bridge tests and the `stub-daemon`
//! example (which drives the Flow bar in the X11 test and for screenshots).
//!
//! Replies are built with `dictate-proto`'s own types, so a wire change breaks
//! this stub at compile time rather than letting it drift.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dictate_proto::{
    Capabilities, CommandResult, ErrorCode, Message, ProtoError, RequestId, ServerHello, ServerInfo,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, watch};

/// A running stub.
pub struct StubDaemon {
    path: PathBuf,
    events: broadcast::Sender<Value>,
    kick: watch::Sender<u64>,
    received: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl StubDaemon {
    /// Listen on `path`.
    pub async fn start(path: &Path) -> Self {
        let _ = std::fs::remove_file(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        let listener = UnixListener::bind(path).expect("bind stub socket");
        let (events, _) = broadcast::channel(4096);
        let (kick, _) = watch::channel(0u64);
        let received = Arc::new(Mutex::new(Vec::new()));
        let task = {
            let events = events.clone();
            let kick = kick.clone();
            let received = received.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else { return };
                    let events = events.subscribe();
                    let kick = kick.subscribe();
                    let received = received.clone();
                    tokio::spawn(serve(stream, events, kick, received));
                }
            })
        };
        Self {
            path: path.to_path_buf(),
            events,
            kick,
            received,
            task,
        }
    }

    /// Send an event (the inner `event` object) to every subscribed client.
    pub fn emit(&self, event: Value) {
        let _ = self.events.send(event);
    }

    /// Emit a raw `state_changed`.
    pub fn state(&self, from: &str, to: &str) {
        self.emit(json!({"type": "state_changed", "session_id": "stub-1", "from": from, "to": to}));
    }

    /// Close every open connection (the listener stays up).
    pub fn drop_connections(&self) {
        self.kick.send_modify(|n| *n += 1);
    }

    /// Every request received, in order.
    pub fn requests(&self) -> Vec<Value> {
        self.received.lock().unwrap().clone()
    }

    /// Stop listening and remove the socket.
    pub fn stop(self) {
        self.drop_connections();
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn serve(
    stream: UnixStream,
    mut events: broadcast::Receiver<Value>,
    mut kick: watch::Receiver<u64>,
    received: Arc<Mutex<Vec<Value>>>,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut subscribed = false;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { return };
                let Ok(request) = serde_json::from_str::<Value>(&line) else { continue };
                received.lock().unwrap().push(request.clone());
                let id = request.get("id").and_then(Value::as_u64).map(RequestId::Number);
                let command = request.pointer("/command/type").and_then(Value::as_str).unwrap_or("");
                if command == "subscribe" {
                    subscribed = true;
                }
                let Some(reply) = answer(command, &request) else { continue };
                let Some(id) = id else { continue };
                let message = match reply {
                    Ok(result) => Message::ok(id, result),
                    Err(error) => Message::err(id, error),
                };
                if write.write_all(message.to_ndjson_line().unwrap().as_bytes()).await.is_err() {
                    return;
                }
            }
            event = events.recv() => {
                let Ok(event) = event else { continue };
                if subscribed {
                    let line = format!("{}\n", json!({"kind": "event", "v": 1, "event": event}));
                    if write.write_all(line.as_bytes()).await.is_err() {
                        return;
                    }
                }
            }
            changed = kick.changed() => {
                if changed.is_ok() {
                    return;
                }
            }
        }
    }
}

/// Canned answers. `None` means "never answer" (for timeout tests).
fn answer(command: &str, request: &Value) -> Option<Result<CommandResult, ProtoError>> {
    Some(match command {
        "handshake" => Ok(CommandResult::Handshake(Box::new(ServerHello {
            protocol_version: dictate_proto::PROTOCOL_VERSION,
            supported_versions: vec![dictate_proto::PROTOCOL_VERSION],
            server: ServerInfo::new("dictated", "0.0.0-stub"),
            capabilities: {
                let mut caps = Capabilities::local_trusted();
                caps.features.config_read = true;
                caps.features.config_write = true;
                caps
            },
        }))),
        "diagnose" => return None,
        "set_config" if request.pointer("/command/entries/0/path") == Some(&json!("grammar.timeout_s")) => {
            let mut e = ProtoError::new(
                ErrorCode::ConfigInvalid,
                "invalid configuration at 'grammar.timeout_s': must be positive",
            );
            e.detail = Some(Box::new(json!({"path": "grammar.timeout_s", "errors": ["must be positive"]})));
            Err(e)
        }
        "toggle" => Ok(CommandResult::SessionStarted {
            session_id: "stub-1".into(),
        }),
        _ => Ok(CommandResult::Ack),
    })
}
