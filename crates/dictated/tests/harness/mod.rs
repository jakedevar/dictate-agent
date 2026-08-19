//! A real daemon on a real socket, with mock hardware.
//!
//! Everything under test here is production code: the engine, the state
//! machine, the NDJSON framing, the dispatcher, the capability gate. Only the
//! four things CI cannot have — a microphone, a GPU, an Ollama, and a desktop
//! to type into — are doubles. That is the point: a control-plane bug should
//! fail these tests, and a test that passed against a mock engine would prove
//! nothing about the daemon Jake runs.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dictate_core::ports::mock::{
    MockAudio, MockFormatter, MockInjector, MockStt, NullMedia, RecordingNotifier,
};
use dictate_core::ports::VoiceActivityGate;
use dictate_core::ports::{Formatter, SttProvider};
use dictate_core::Pipeline;
use dictate_history::{HistoryConfig, HistoryStore};
use dictate_proto::{
    Capabilities, Command, CommandResult, Event, Message, ProtoError, RequestId, ServerHello,
    State, Transcript,
};
use dictated::paths::RuntimePaths;
use dictated::Daemon;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;

/// Every await in these tests is bounded. A control-plane bug should fail as a
/// clear assertion, not as a suite that hangs until CI times out.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Wrap a future in the suite's timeout.
pub async fn within<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(TIMEOUT, fut).await {
        Ok(v) => v,
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

/// How a harness should be configured.
pub struct Setup {
    pub stt: Arc<dyn SttProvider>,
    pub injector: Arc<MockInjector>,
    pub audio: Arc<MockAudio>,
    pub formatter: Arc<dyn Formatter>,
    pub vad: Arc<dyn VoiceActivityGate>,
    pub capabilities: Capabilities,
    pub history_enabled: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            stt: Arc::new(MockStt::returning("hello there")),
            injector: Arc::new(MockInjector::new()),
            audio: Arc::new(MockAudio::with_seconds(1.5)),
            // Appends nothing, but *runs*, so the formatting stage is exercised.
            formatter: Arc::new(MockFormatter::default()),
            vad: Arc::new(
                dictate_vad::SileroVad::new(dictate_vad::VadConfig {
                    enabled: false,
                    ..Default::default()
                })
                .unwrap(),
            ),
            capabilities: dictated::server::local_capabilities(true),
            history_enabled: false,
        }
    }
}

impl Setup {
    pub fn with_stt(mut self, stt: Arc<dyn SttProvider>) -> Self {
        self.stt = stt;
        self
    }
    pub fn with_injector(mut self, injector: Arc<MockInjector>) -> Self {
        self.injector = injector;
        self
    }
    pub fn with_audio(mut self, audio: Arc<MockAudio>) -> Self {
        self.audio = audio;
        self
    }
    pub fn with_formatter(mut self, formatter: Arc<dyn Formatter>) -> Self {
        self.formatter = formatter;
        self
    }
    pub fn with_vad(mut self, vad: Arc<dyn VoiceActivityGate>) -> Self {
        self.vad = vad;
        self
    }
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
    pub fn with_history(mut self) -> Self {
        self.history_enabled = true;
        self
    }
}

/// A running daemon plus the doubles it was built from.
pub struct Harness {
    pub daemon: Daemon,
    pub socket: PathBuf,
    pub audio: Arc<MockAudio>,
    pub injector: Arc<MockInjector>,
    pub notifier: Arc<RecordingNotifier>,
    pub history: Arc<Mutex<HistoryStore>>,
    dir: PathBuf,
}

impl Harness {
    pub async fn start() -> Self {
        Self::with(Setup::default()).await
    }

    pub async fn with(setup: Setup) -> Self {
        // A unique directory per harness. This must be a process-wide counter,
        // not a thread id: these tests run on multi-threaded runtimes, so a
        // thread id is neither stable within a test nor unique across them —
        // and the `remove_dir_all` below would then delete a *concurrent*
        // harness's socket out from under it.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dictated-it-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let history = Arc::new(Mutex::new(
            HistoryStore::new(&HistoryConfig {
                enabled: setup.history_enabled,
                db_path: dir.join("history.db").to_string_lossy().into_owned(),
                max_response_length: 10_000,
            })
            .unwrap(),
        ));
        let notifier = Arc::new(RecordingNotifier::default());

        let pipeline = Arc::new(Pipeline {
            audio: setup.audio.clone(),
            stt: setup.stt.clone(),
            vad: setup.vad.clone(),
            formatter: setup.formatter.clone(),
            injector: setup.injector.clone(),
            notifier: notifier.clone(),
            media: Arc::new(NullMedia),
            history: history.clone(),
            local: Arc::new(dictate_core::local_executor::LocalExecutor::new(
                &dictate_core::config::LocalConfig::default(),
            )),
            timer: Arc::new(dictate_core::timer::TimerExecutor::new(
                &dictate_core::config::TimerConfig::default(),
            )),
            local_model: "mock".into(),
        });

        let runtime = RuntimePaths::under(&dir);
        let daemon = Daemon::start(
            pipeline,
            history.clone(),
            &runtime,
            setup.capabilities,
            None,
        )
        .await
        .expect("daemon must start");

        Self {
            socket: daemon.socket().to_path_buf(),
            daemon,
            audio: setup.audio,
            injector: setup.injector,
            notifier,
            history,
            dir,
        }
    }

    /// Connect a client and complete the handshake.
    pub async fn client(&self) -> Client {
        let mut c = Client::connect(&self.socket).await;
        c.handshake().await;
        c
    }

    /// Connect a client without handshaking.
    pub async fn raw_client(&self) -> Client {
        Client::connect(&self.socket).await
    }

    pub async fn stop(self) {
        let dir = self.dir.clone();
        self.daemon.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A protocol client that demultiplexes responses from events.
///
/// Both travel down the same socket, so a client that read blindly would
/// consume an event while waiting for its response. This buffers events it was
/// not asking for, which is exactly what a real client (and the Tauri UI) has
/// to do.
pub struct Client {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
    pending_events: std::collections::VecDeque<Event>,
    /// Every state this connection has been told about, in order.
    ///
    /// Recorded as events arrive rather than reconstructed by a waiter, so an
    /// assertion about the stage walk is independent of which helper consumed
    /// which event. `wait_for_final` would otherwise silently eat the
    /// transitions a later `wait_for_state` wanted to see.
    seen_states: Vec<State>,
    pub hello: Option<ServerHello>,
}

impl Client {
    pub async fn connect(path: &std::path::Path) -> Self {
        let stream = UnixStream::connect(path).await.expect("connect");
        let (read, write) = stream.into_split();
        Self {
            reader: BufReader::new(read),
            writer: write,
            next_id: 1,
            pending_events: std::collections::VecDeque::new(),
            seen_states: Vec::new(),
            hello: None,
        }
    }

    async fn send(&mut self, message: &Message) {
        let line = message.to_ndjson_line().unwrap();
        self.writer.write_all(line.as_bytes()).await.expect("write");
        self.writer.flush().await.expect("flush");
    }

    async fn read_message(&mut self) -> Option<Message> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await.ok()?;
        if n == 0 {
            return None;
        }
        Some(serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad frame {line:?}: {e}")))
    }

    /// Send a command and wait for *its* response, buffering events meanwhile.
    pub async fn request(&mut self, command: Command) -> Result<CommandResult, ProtoError> {
        let id = RequestId::Number(self.next_id);
        self.next_id += 1;
        self.send(&Message::request(id.clone(), command)).await;

        within("a response", async {
            loop {
                match self.read_message().await.expect("connection closed") {
                    Message::Response(r) if r.id == id => {
                        return match r.outcome {
                            dictate_proto::Outcome::Result(v) => Ok(v),
                            dictate_proto::Outcome::Error(e) => Err(e),
                        }
                    }
                    Message::Event(e) => {
                        if let Event::StateChanged { to, .. } = &e.event {
                            self.seen_states.push(to.clone());
                        }
                        self.pending_events.push_back(e.event);
                    }
                    Message::Response(other) => panic!("response for a foreign id {:?}", other.id),
                    Message::Request(_) => panic!("the daemon sent us a request"),
                }
            }
        })
        .await
    }

    pub async fn handshake(&mut self) -> ServerHello {
        let hello = dictate_proto::Hello::new(dictate_proto::ClientInfo::new(
            "test-client",
            dictate_proto::ClientKind::Cli,
        ));
        match self
            .request(Command::Handshake(hello))
            .await
            .expect("handshake")
        {
            CommandResult::Handshake(h) => {
                self.hello = Some(*h.clone());
                *h
            }
            other => panic!("expected a handshake result, got {other:?}"),
        }
    }

    pub async fn subscribe(&mut self) {
        assert!(matches!(
            self.request(Command::Subscribe { events: Vec::new() })
                .await
                .expect("subscribe"),
            CommandResult::Ack
        ));
    }

    /// The next event, from the buffer or the socket.
    pub async fn next_event(&mut self) -> Event {
        let event = match self.pending_events.pop_front() {
            Some(e) => e,
            None => {
                within("an event", async {
                    match self.read_message().await.expect("connection closed") {
                        Message::Event(e) => e.event,
                        Message::Response(r) => panic!("unexpected response {:?}", r.id),
                        Message::Request(_) => panic!("the daemon sent us a request"),
                    }
                })
                .await
            }
        };
        if let Event::StateChanged { to, .. } = &event {
            self.seen_states.push(to.clone());
        }
        event
    }

    /// Every state this connection has observed so far.
    pub fn seen_states(&self) -> &[State] {
        &self.seen_states
    }

    /// Assert the session walked through `states`, in order (gaps allowed).
    pub fn assert_walked(&self, states: &[State]) {
        let mut remaining = states.iter();
        let mut want = remaining.next();
        for seen in &self.seen_states {
            if Some(seen) == want {
                want = remaining.next();
            }
        }
        assert!(
            want.is_none(),
            "expected the walk {states:?}, but saw {:?}",
            self.seen_states
        );
    }

    /// Consume events until the session reaches `state`.
    ///
    /// Returns the transitions consumed by *this* call; use
    /// [`Client::seen_states`] for the connection's whole history.
    pub async fn wait_for_state(&mut self, state: State) -> Vec<State> {
        let mut seen = Vec::new();
        within(&format!("state {}", state.as_str()), async {
            loop {
                if let Event::StateChanged { to, .. } = self.next_event().await {
                    seen.push(to.clone());
                    if to == state {
                        return seen;
                    }
                }
            }
        })
        .await
    }

    /// Consume events until a `final` arrives.
    pub async fn wait_for_final(&mut self) -> Transcript {
        within("a final event", async {
            loop {
                if let Event::Final { transcript, .. } = self.next_event().await {
                    return *transcript;
                }
            }
        })
        .await
    }

    /// Consume events until a terminal state is reached, returning it.
    pub async fn wait_for_terminal(&mut self) -> State {
        within("a terminal state", async {
            loop {
                if let Event::StateChanged { to, .. } = self.next_event().await {
                    if to.is_terminal() {
                        return to;
                    }
                }
            }
        })
        .await
    }

    /// Drop the connection, which is what the engine sees when a CLI exits.
    pub async fn disconnect(self) {
        drop(self);
        // Give the daemon a moment to observe the close and process the
        // resulting `Disconnected` message.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll `check` until it holds or the timeout expires.
pub async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    within(what, async {
        loop {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
}
