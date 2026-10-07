//! The protocol bridge against a stub daemon on a real unix socket.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::stub::StubDaemon;
use dictate_proto::{Command, ConfigEntry};
use dictate_ui::bridge::{Bridge, BridgeConfig, Connection, EventSink};
use serde_json::{json, Value};
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Recorder {
    connections: Mutex<Vec<Connection>>,
    events: Mutex<Vec<Value>>,
    changed: Notify,
}

impl EventSink for Recorder {
    fn connection(&self, state: &Connection) {
        self.connections.lock().unwrap().push(state.clone());
        self.changed.notify_waiters();
    }
    fn event(&self, event: &Value) {
        self.events.lock().unwrap().push(event.clone());
        self.changed.notify_waiters();
    }
}

impl Recorder {
    async fn until(&self, what: &str, done: impl Fn(&Self) -> bool) {
        tokio::time::timeout(WAIT, async {
            loop {
                let notified = self.changed.notified();
                if done(self) {
                    return;
                }
                let _ = tokio::time::timeout(Duration::from_millis(50), notified).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    fn last_connection(&self) -> Option<Connection> {
        self.connections.lock().unwrap().last().cloned()
    }

    fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }
}

/// A short socket path: `sockaddr_un` caps paths at 108 bytes and the RSI
/// harness exports a long `TMPDIR`.
fn socket() -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    PathBuf::from(format!("/tmp/dui-{}-{n}/d.sock", std::process::id()))
}

fn config(socket: PathBuf) -> BridgeConfig {
    BridgeConfig {
        socket,
        backoff_min: Duration::from_millis(20),
        backoff_max: Duration::from_millis(80),
        handshake_timeout: Duration::from_secs(2),
        request_timeout: Duration::from_secs(5),
        level_interval: Duration::from_millis(33),
    }
}

fn start(socket: PathBuf) -> (Bridge, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let (bridge, task) = Bridge::new(config(socket), recorder.clone());
    tokio::spawn(task);
    (bridge, recorder)
}

async fn connected(recorder: &Recorder) {
    recorder
        .until("a connection", |r| {
            r.last_connection().is_some_and(|c| c.is_connected())
        })
        .await;
}

#[tokio::test]
async fn handshakes_as_the_desktop_ui_and_subscribes_to_everything() {
    let path = socket();
    let stub = StubDaemon::start(&path).await;
    let (bridge, recorder) = start(path);
    connected(&recorder).await;

    let requests = stub.requests();
    assert_eq!(requests[0]["command"]["type"], "handshake");
    assert_eq!(requests[0]["command"]["client"]["kind"], "desktop_ui");
    assert_eq!(requests[0]["command"]["client"]["name"], "dictate-ui");
    assert_eq!(requests[1]["command"]["type"], "subscribe");
    assert!(
        requests[1]["command"].get("events").is_none(),
        "an empty filter subscribes to every event, including ones this build does not know"
    );
    match bridge.connection() {
        Connection::Connected {
            server,
            protocol_version,
            features,
            ..
        } => {
            assert_eq!(server, "dictated");
            assert_eq!(protocol_version, dictate_proto::PROTOCOL_VERSION);
            assert!(features.config_write);
        }
        other => panic!("expected connected, got {other:?}"),
    }
    stub.stop();
}

#[tokio::test]
async fn results_and_errors_pass_through_intact() {
    let path = socket();
    let stub = StubDaemon::start(&path).await;
    let (bridge, recorder) = start(path);
    connected(&recorder).await;

    let result = bridge.request(Command::Toggle).await.unwrap();
    assert_eq!(result, json!({"type": "session_started", "session_id": "stub-1"}));

    let err = bridge
        .request(Command::SetConfig {
            entries: vec![ConfigEntry::new("grammar.timeout_s", json!(0))],
            document: None,
            dry_run: false,
            base_revision: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, "config_invalid");
    assert!(err.message.contains("grammar.timeout_s"));
    assert_eq!(err.detail.unwrap()["path"], "grammar.timeout_s", "detail survives for the UI to highlight the field");

    // The command reached the daemon in its protocol shape.
    let sent = stub.requests();
    let set = sent.iter().find(|r| r["command"]["type"] == "set_config").unwrap();
    assert_eq!(set["command"]["entries"][0]["path"], "grammar.timeout_s");
    stub.stop();
}

#[tokio::test]
async fn events_are_relayed_raw_including_ones_this_build_does_not_know() {
    let path = socket();
    let stub = StubDaemon::start(&path).await;
    let (_bridge, recorder) = start(path);
    connected(&recorder).await;

    stub.state("idle", "recording");
    let future = json!({"type": "wake_word_heard", "session_id": "s", "keyword": "hey", "extra": {"x": 1}});
    stub.emit(future.clone());
    recorder.until("two events", |r| r.events().len() >= 2).await;
    let events = recorder.events();
    assert_eq!(events[0]["to"], "recording");
    assert_eq!(events[1], future, "a relay must not flatten unknown events");
    stub.stop();
}

#[tokio::test]
async fn audio_levels_are_throttled_and_keep_the_peak() {
    let path = socket();
    let stub = StubDaemon::start(&path).await;
    let (_bridge, recorder) = start(path);
    connected(&recorder).await;

    // A burst of 60 levels far faster than 30 fps, loudest in the middle.
    for i in 0..60 {
        let rms = if i == 30 { 0.95 } else { 0.1 };
        stub.emit(json!({"type": "audio_level", "session_id": "s", "rms": rms}));
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    let levels: Vec<Value> = recorder
        .events()
        .into_iter()
        .filter(|e| e["type"] == "audio_level")
        .collect();
    assert!(
        (1..=6).contains(&levels.len()),
        "60 levels in a few ms must collapse to a handful, got {}",
        levels.len()
    );
    assert!(
        levels.iter().any(|e| e["rms"] == json!(0.95)),
        "the peak must survive coalescing: {levels:?}"
    );
    stub.stop();
}

#[tokio::test]
async fn a_missing_daemon_is_reported_and_requests_fail_fast() {
    let path = socket();
    let (bridge, recorder) = start(path.clone());
    recorder
        .until("disconnected", |r| {
            matches!(r.last_connection(), Some(Connection::Disconnected { .. }))
        })
        .await;
    match bridge.connection() {
        Connection::Disconnected { reason, .. } => {
            assert!(reason.contains("no daemon listening"), "{reason}");
            assert!(reason.contains(&path.display().to_string()), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    let started = std::time::Instant::now();
    let err = bridge.request(Command::GetStatus).await.unwrap_err();
    assert_eq!(err.code, "daemon_unavailable");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "never queued or left hanging"
    );
}

#[tokio::test]
async fn it_reconnects_when_the_daemon_appears_and_after_it_drops() {
    let path = socket();
    let (bridge, recorder) = start(path.clone());
    recorder
        .until("a failed attempt", |r| {
            matches!(r.last_connection(), Some(Connection::Disconnected { .. }))
        })
        .await;

    // The daemon starts later: the bridge finds it on its own.
    let stub = StubDaemon::start(&path).await;
    connected(&recorder).await;

    // An in-flight request when the connection drops fails as connection_lost.
    let pending = {
        let bridge = bridge.clone();
        tokio::spawn(async move {
            bridge
                .request_within(Command::Diagnose { quick: true }, Duration::from_secs(5))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let before = recorder.connections.lock().unwrap().len();
    stub.drop_connections();
    let err = pending.await.unwrap().unwrap_err();
    assert_eq!(err.code, "connection_lost");

    // ...and the bridge comes back by itself, subscribed again.
    recorder
        .until("a reconnection", |r| {
            let all = r.connections.lock().unwrap();
            all.len() > before + 1 && all.last().is_some_and(Connection::is_connected)
        })
        .await;
    stub.state("idle", "recording");
    recorder
        .until("events after reconnecting", |r| !r.events().is_empty())
        .await;
    let handshakes = stub
        .requests()
        .iter()
        .filter(|r| r["command"]["type"] == "subscribe")
        .count();
    assert_eq!(handshakes, 2, "resubscribed on the new connection");
    stub.stop();
}

#[tokio::test]
async fn an_unanswered_request_times_out_and_the_connection_survives() {
    let path = socket();
    let stub = StubDaemon::start(&path).await;
    let (bridge, recorder) = start(path);
    connected(&recorder).await;
    let err = bridge
        .request_within(Command::Diagnose { quick: true }, Duration::from_millis(150))
        .await
        .unwrap_err();
    assert_eq!(err.code, "timeout");
    assert!(bridge.request(Command::GetStatus).await.is_ok());
    stub.stop();
}

#[tokio::test]
async fn dropping_every_handle_ends_the_task() {
    let path = socket();
    let stub = StubDaemon::start(&path).await;
    let recorder = Arc::new(Recorder::default());
    let (bridge, task) = Bridge::new(config(path), recorder.clone());
    let task = tokio::spawn(task);
    connected(&recorder).await;
    drop(bridge);
    tokio::time::timeout(WAIT, task)
        .await
        .expect("the bridge task must exit once nobody can use it")
        .unwrap();
    stub.stop();
}
