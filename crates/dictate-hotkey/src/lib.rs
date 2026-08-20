//! Linux global hotkeys backed by evdev.
//!
//! One OS thread owns every opened input device. It never calls
//! the engine directly: it emits small [`HotkeyAction`] messages to a Tokio
//! task, which then uses the engine's existing single-writer mailbox. This
//! keeps physical hotkeys and SIGUSR1/2 on one state-machine path.

use std::collections::BTreeSet;
use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use dictate_core::config::HotkeyConfig;
use dictate_core::session::Actor;
use dictate_core::{EngineHandle, ResolvedOptions};
use dictate_proto::{Event, State};
use evdev::{Device, EventSummary};
use tokio::sync::mpsc as tokio_mpsc;
use tracing::{debug, info, warn};

/// Action normalized by the evdev owner and applied on the daemon runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    Start,
    Stop,
    Toggle,
    Cancel,
}

impl HotkeyAction {
    async fn apply(self, engine: &EngineHandle, options: &ResolvedOptions) {
        let result = match self {
            Self::Start => engine
                .start(
                    Actor::Signal,
                    dictate_proto::DictationMode::PushToTalk,
                    options.clone(),
                )
                .await
                .map(|_| ()),
            Self::Stop => engine.stop(Actor::Signal).await.map(|_| ()),
            Self::Toggle => engine
                .toggle(Actor::Signal, options.clone())
                .await
                .map(|_| ()),
            Self::Cancel => engine.cancel(Actor::Signal).await.map(|_| ()),
        };
        if let Err(error) = result {
            // A second key event arriving after the engine has advanced is a
            // normal race, never a reason to create a second control path.
            debug!(action = ?self, code = error.code.as_str(), "hotkey action refused");
        }
    }
}

/// Lifetime handle for the optional evdev service.
pub struct HotkeyService {
    control: Option<mpsc::Sender<ManagerControl>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl HotkeyService {
    /// Start the configured service. Failure to read an input node is a
    /// logged degradation, not a daemon-start failure: WM signal keybindings
    /// remain fully operational without membership in the `input` group.
    #[must_use]
    pub fn start(config: &HotkeyConfig, engine: EngineHandle, options: ResolvedOptions) -> Self {
        if !config.enabled {
            info!("evdev hotkey disabled; WM/signal controls remain active");
            return Self {
                control: None,
                thread: None,
            };
        }
        if config.devices.is_empty() {
            warn!("evdev hotkey enabled without devices; degrading to WM/signal controls");
            return Self {
                control: None,
                thread: None,
            };
        }

        let (control_tx, control_rx) = mpsc::channel();
        let unlock_tx = control_tx.clone();
        let mut state_events = engine.subscribe();
        tokio::spawn(async move {
            while let Ok(event) = state_events.recv().await {
                if matches!(
                    event,
                    Event::StateChanged {
                        to: State::Done | State::Error | State::Cancelled | State::Idle,
                        ..
                    }
                ) {
                    // The evdev owner alone mutates its state. A completed
                    // hands-free session must not leave it permanently locked.
                    let _ = unlock_tx.send(ManagerControl::Unlock);
                }
            }
        });

        let (actions_tx, mut actions_rx) = tokio_mpsc::channel::<HotkeyAction>(32);
        tokio::spawn(async move {
            while let Some(action) = actions_rx.recv().await {
                action.apply(&engine, &options).await;
            }
        });

        let manager = Manager::from_config(config, actions_tx);
        let devices = config.devices.clone();
        let thread = match thread::Builder::new()
            .name("dictate-hotkey".into())
            .spawn(move || manager.run(devices, control_rx))
        {
            Ok(thread) => thread,
            Err(error) => {
                warn!(%error, "could not spawn evdev hotkey thread");
                return Self {
                    control: None,
                    thread: None,
                };
            }
        };
        Self {
            control: Some(control_tx),
            thread: Some(thread),
        }
    }

    /// Ask the input owner to exit and wait for it. It is safe to call repeatedly.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(control) = self.control.take() {
            let _ = control.send(ManagerControl::Shutdown);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HotkeyService {
    fn drop(&mut self) {
        self.stop();
    }
}

enum ManagerControl {
    Shutdown,
    Unlock,
}

/// The pure state machine that the single device-owner thread drives.
struct Manager {
    hold: BTreeSet<u16>,
    toggle: BTreeSet<u16>,
    cancel: BTreeSet<u16>,
    pressed: BTreeSet<u16>,
    holding: bool,
    locked: bool,
    release_due: Option<Instant>,
    last_release: Option<Instant>,
    toggle_latched: bool,
    cancel_latched: bool,
    release_grace: Duration,
    double_tap: Duration,
    actions: tokio_mpsc::Sender<HotkeyAction>,
}

impl Manager {
    fn from_config(config: &HotkeyConfig, actions: tokio_mpsc::Sender<HotkeyAction>) -> Self {
        Self {
            hold: config.hold_to_talk.iter().copied().collect(),
            toggle: config.toggle.iter().copied().collect(),
            cancel: config.cancel.iter().copied().collect(),
            pressed: BTreeSet::new(),
            holding: false,
            locked: false,
            release_due: None,
            last_release: None,
            toggle_latched: false,
            cancel_latched: false,
            release_grace: Duration::from_millis(config.release_grace_ms),
            double_tap: Duration::from_millis(config.double_tap_ms),
            actions,
        }
    }

    fn matches(chord: &BTreeSet<u16>, pressed: &BTreeSet<u16>) -> bool {
        !chord.is_empty() && chord.is_subset(pressed)
    }

    fn emit(&self, action: HotkeyAction) {
        if let Err(error) = self.actions.try_send(action) {
            warn!(?action, %error, "dropping hotkey action because the engine queue is unavailable");
        }
    }

    fn key(&mut self, code: u16, value: i32, now: Instant) {
        // Resolve an expired deferred release before interpreting a later key
        // event. This keeps a press arriving just after the double-tap window
        // from accidentally cancelling a stop that the polling tick has not
        // observed yet.
        self.tick(now);
        if value == 0 {
            self.pressed.remove(&code);
        } else {
            self.pressed.insert(code);
        }

        let cancel = Self::matches(&self.cancel, &self.pressed);
        if cancel && !self.cancel_latched {
            self.unlock();
            self.emit(HotkeyAction::Cancel);
        }
        self.cancel_latched = cancel;
        if cancel {
            return;
        }

        let toggle = Self::matches(&self.toggle, &self.pressed);
        if toggle && value == 1 && !self.toggle_latched {
            self.emit(HotkeyAction::Toggle);
        }
        self.toggle_latched = toggle;

        let hold_matches = Self::matches(&self.hold, &self.pressed);
        if hold_matches && value != 0 {
            if self.release_due.take().is_some() {
                // A hardware auto-repeat reports value=2. It must retain the
                // current hold, but it must never turn a hold into a lock.
                if value == 1
                    && self
                        .last_release
                        .is_some_and(|at| now.duration_since(at) <= self.double_tap)
                {
                    self.locked = true;
                    self.holding = false;
                    info!("hotkey double-tap locked hands-free dictation");
                }
            } else if !self.holding && !self.locked {
                self.holding = true;
                self.emit(HotkeyAction::Start);
            }
        }
        if self.holding && !hold_matches && value == 0 {
            self.release_due = Some(now + self.release_grace.max(self.double_tap));
            self.last_release = Some(now);
        }
    }

    fn tick(&mut self, now: Instant) {
        if self.release_due.is_some_and(|due| now >= due) {
            self.release_due = None;
            if self.holding && !self.locked {
                self.holding = false;
                self.emit(HotkeyAction::Stop);
            }
        }
    }

    fn unlock(&mut self) {
        self.holding = false;
        self.locked = false;
        self.release_due = None;
        self.last_release = None;
    }

    fn run(self, paths: Vec<String>, control: mpsc::Receiver<ManagerControl>) {
        self.run_inner(paths, control);
    }

    fn run_inner(mut self, paths: Vec<String>, control: mpsc::Receiver<ManagerControl>) {
        let mut devices = Vec::new();
        for path in paths {
            match Device::open(&path) {
                Ok(device) => {
                    // EVIOCGRAB is deliberately not used: it would suppress
                    // every key from the selected keyboard in the compositor.
                    // Read access is sufficient for global chord observation.
                    if let Err(error) = device.set_nonblocking(true) {
                        warn!(%error, %path, "cannot make hotkey device nonblocking");
                    }
                    devices.push(device);
                }
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                    warn!(%path, "cannot read evdev device (add this user to input group); degrading to WM/signal controls")
                }
                Err(error) => {
                    warn!(%error, %path, "cannot open evdev device; degrading to WM/signal controls")
                }
            }
        }
        if devices.is_empty() {
            return;
        }
        info!(count = devices.len(), "evdev hotkey service active");
        loop {
            match control.try_recv() {
                Ok(ManagerControl::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => break,
                Ok(ManagerControl::Unlock) => self.unlock(),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            for device in &mut devices {
                match device.fetch_events() {
                    Ok(events) => {
                        for event in events {
                            if let EventSummary::Key(_, key, value) = event.destructure() {
                                self.key(key.0, value, Instant::now());
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => warn!(%error, "evdev read failed"),
                }
            }
            self.tick(Instant::now());
            thread::sleep(Duration::from_millis(3));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use dictate_core::engine::DaemonIdentity;
    use dictate_core::ports::mock::{
        MockAudio, MockFormatter, MockInjector, MockStt, MockVad, NullEarcons, NullMedia,
        RecordingNotifier,
    };
    use dictate_core::{Engine, EventBus, Pipeline};
    use evdev::uinput::VirtualDevice;
    use evdev::{AttributeSet, InputEvent, KeyCode};
    use tokio::sync::mpsc as tokio_mpsc;

    fn manager() -> (Manager, tokio_mpsc::Receiver<HotkeyAction>) {
        let config = HotkeyConfig {
            hold_to_talk: vec![57],
            toggle: vec![58],
            cancel: vec![29, 56],
            ..HotkeyConfig::default()
        };
        let (tx, rx) = tokio_mpsc::channel(8);
        (Manager::from_config(&config, tx), rx)
    }

    #[tokio::test]
    async fn a_hold_release_drives_start_then_stop() {
        let (mut manager, mut actions) = manager();
        let now = Instant::now();
        manager.key(57, 1, now);
        manager.key(57, 0, now + Duration::from_millis(5));
        manager.tick(now + Duration::from_millis(400));
        assert_eq!(actions.recv().await, Some(HotkeyAction::Start));
        assert_eq!(actions.recv().await, Some(HotkeyAction::Stop));
    }

    #[tokio::test]
    async fn repeat_during_release_grace_does_not_flap_or_lock() {
        let (mut manager, mut actions) = manager();
        let now = Instant::now();
        manager.key(57, 1, now);
        manager.key(57, 0, now + Duration::from_millis(5));
        manager.key(57, 2, now + Duration::from_millis(10));
        manager.tick(now + Duration::from_millis(100));
        assert_eq!(actions.recv().await, Some(HotkeyAction::Start));
        assert!(actions.try_recv().is_err());
        assert!(!manager.locked);
    }

    #[tokio::test]
    async fn double_tap_locks_and_release_does_not_stop() {
        let (mut manager, mut actions) = manager();
        let now = Instant::now();
        manager.key(57, 1, now);
        manager.key(57, 0, now + Duration::from_millis(5));
        // The lock window remains effective after the short release debounce.
        manager.key(57, 1, now + Duration::from_millis(150));
        manager.key(57, 0, now + Duration::from_millis(155));
        manager.tick(now + Duration::from_millis(400));
        assert_eq!(actions.recv().await, Some(HotkeyAction::Start));
        assert!(actions.try_recv().is_err());
        assert!(manager.locked);
    }

    #[tokio::test]
    async fn cancel_is_statically_available_without_reconfiguring_devices() {
        let (mut manager, mut actions) = manager();
        let now = Instant::now();
        manager.key(29, 1, now);
        manager.key(56, 1, now);
        assert_eq!(actions.recv().await, Some(HotkeyAction::Cancel));
    }

    #[test]
    fn completing_a_locked_session_reenables_hold_to_talk() {
        let (mut manager, _actions) = manager();
        manager.holding = true;
        manager.locked = true;
        manager.release_due = Some(Instant::now());
        manager.last_release = Some(Instant::now());

        manager.unlock();

        assert!(!manager.holding);
        assert!(!manager.locked);
        assert!(manager.release_due.is_none());
        assert!(manager.last_release.is_none());
    }

    #[test]
    fn permission_missing_is_a_nonfatal_degradation() {
        let (manager, _actions) = manager();
        let (control_tx, control_rx) = std::sync::mpsc::channel();
        control_tx.send(ManagerControl::Shutdown).unwrap();
        manager.run(vec!["/definitely/not/an/input-device".into()], control_rx);
    }

    /// This is the kernel-facing half of the transition coverage. CI runners
    /// without access to `/dev/uinput` still exercise the same state machine
    /// above; an input-enabled runner additionally proves the actual evdev
    /// read loop receives the generated press/release pair.
    #[tokio::test(flavor = "multi_thread")]
    async fn uinput_events_drive_real_engine_state_transitions() {
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::KEY_SPACE);
        let mut virtual_device = match VirtualDevice::builder()
            .and_then(|builder| builder.name(&"dictate-hotkey-test").with_keys(&keys))
            .and_then(|builder| builder.build())
        {
            Ok(device) => device,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("create uinput test device: {error}"),
        };
        let path = virtual_device
            .enumerate_dev_nodes_blocking()
            .expect("enumerate uinput device")
            .next()
            .expect("uinput device node")
            .expect("uinput node path");
        // Creating uinput can be permitted while opening the resulting
        // `/dev/input/event*` node is still denied to this user. That is the
        // exact production degradation path, so leave the state-machine unit
        // tests to cover it and run this kernel integration test only where
        // input read permission is available.
        if Device::open(&path).is_err() {
            return;
        }

        let history = Arc::new(Mutex::new(
            dictate_history::HistoryStore::new(&dictate_history::HistoryConfig {
                enabled: false,
                ..Default::default()
            })
            .unwrap(),
        ));
        let pipeline = Arc::new(Pipeline {
            audio: Arc::new(MockAudio::with_seconds(0.1)),
            stt: Arc::new(MockStt::returning("hotkey transition")),
            vad: Arc::new(MockVad::returning(dictate_core::ports::GateDecision::Speech {
                samples: vec![0.1; 1_600],
                leading_trimmed_ms: 0.0,
                trailing_trimmed_ms: 0.0,
            })),
            formatter: Arc::new(MockFormatter::disabled()),
            injector: Arc::new(MockInjector::unavailable()),
            notifier: Arc::new(RecordingNotifier::default()),
            media: Arc::new(NullMedia),
            earcons: Arc::new(NullEarcons),
            history,
            local: Arc::new(dictate_core::local_executor::LocalExecutor::new(
                &dictate_core::config::LocalConfig::default(),
            )),
            timer: Arc::new(dictate_core::timer::TimerExecutor::new(
                &dictate_core::config::TimerConfig::default(),
            )),
            local_model: "mock".into(),
        });
        let bus = EventBus::default();
        let mut events = bus.subscribe();
        let (engine, handle) = Engine::new(pipeline, bus, DaemonIdentity::default());
        let engine_task = tokio::spawn(engine.run());

        let config = HotkeyConfig {
            enabled: true,
            devices: vec![path.to_string_lossy().into_owned()],
            hold_to_talk: vec![KeyCode::KEY_SPACE.0],
            release_grace_ms: 5,
            double_tap_ms: 5,
            ..HotkeyConfig::default()
        };
        let service = HotkeyService::start(
            &config,
            handle.clone(),
            ResolvedOptions {
                inject: false,
                ..Default::default()
            },
        );
        thread::sleep(Duration::from_millis(20));
        virtual_device
            .emit(&[InputEvent::new(1, KeyCode::KEY_SPACE.0, 1)])
            .expect("emit press");
        wait_for_state(&mut events, State::Recording).await;
        virtual_device
            .emit(&[InputEvent::new(1, KeyCode::KEY_SPACE.0, 0)])
            .expect("emit release");
        wait_for_state(&mut events, State::Done).await;

        service.shutdown();
        handle.shutdown().await;
        engine_task.await.expect("engine task");
    }

    async fn wait_for_state(
        events: &mut tokio::sync::broadcast::Receiver<Event>,
        expected: State,
    ) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(
                    events.recv().await,
                    Ok(Event::StateChanged { to, .. }) if to == expected
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("engine did not reach {expected:?}"));
    }
}
