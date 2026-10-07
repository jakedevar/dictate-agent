use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt, Window};
use x11rb::rust_connection::RustConnection;

use crate::{ContextProvider, WindowInfo};

/// A single dedicated worker and a bounded queue. A hung X server can consume
/// at most this one thread, never an async worker or an unbounded blocking pool.
/// No logging on missing focus or display failures: these are normal states.
pub struct X11Context {
    requests: SyncSender<Request>,
    connects: Arc<AtomicUsize>,
}
struct Request {
    deadline: Instant,
    reply: SyncSender<Option<WindowInfo>>,
}
const BUDGET: Duration = Duration::from_millis(8);

impl X11Context {
    /// Explicit display, also used by isolated Xvfb tests. Never changes DISPLAY.
    ///
    /// The worker connects **eagerly**, as soon as it starts, so the first
    /// session after daemon start does not spend its 8 ms budget on the
    /// handshake and lose its context. The connection is kept across
    /// per-window X errors (`BadWindow` on a window that is closing); only a
    /// connection-level failure drops it, and the next request reconnects.
    pub fn new(display: String) -> Self {
        let connects = Arc::new(AtomicUsize::new(0));
        let counter = connects.clone();
        Self::worker(connects, move || {
            let connect = move |display: &str| {
                let c = X11Connection::connect(display).ok();
                if c.is_some() {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                c
            };
            let mut connection = connect(&display);
            move || {
                if connection.is_none() {
                    connection = connect(&display);
                }
                match connection.as_ref()?.capture() {
                    Ok(window) => window,
                    Err(_) => {
                        // Connection-level failure: reconnect on the next request.
                        connection = None;
                        None
                    }
                }
            }
        })
    }
    /// How many X connections the worker has established (1 after a healthy
    /// start; more only after a connection-level failure).
    pub fn connection_count(&self) -> usize {
        self.connects.load(Ordering::Relaxed)
    }
    /// Spawn the worker. `make` runs on the worker thread before the first
    /// request is read, and returns the capture function.
    fn worker<C>(connects: Arc<AtomicUsize>, make: impl FnOnce() -> C + Send + 'static) -> Self
    where
        C: FnMut() -> Option<WindowInfo>,
    {
        let (requests, rx) = sync_channel::<Request>(1);
        // Failure to create the worker leaves a disconnected sender. capture()
        // then returns None immediately, preserving the no-context path.
        let _ = std::thread::Builder::new()
            .name("dictate-context-x11".into())
            .spawn(move || {
                let mut capture = make();
                while let Ok(request) = rx.recv() {
                    if Instant::now() >= request.deadline {
                        continue;
                    }
                    let result = capture();
                    let _ = request.reply.try_send(result);
                }
            });
        Self { requests, connects }
    }
}
impl ContextProvider for X11Context {
    fn capture(&self) -> Option<WindowInfo> {
        let deadline = Instant::now() + BUDGET;
        let (reply, rx) = sync_channel(1);
        self.requests.try_send(Request { deadline, reply }).ok()?;
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()
            .flatten()
    }
}

struct X11Connection {
    conn: RustConnection,
    root: Window,
    active: Atom,
    name: Atom,
    utf8: Atom,
    pid: Atom,
}
type XResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
impl X11Connection {
    fn connect(display: &str) -> XResult<Self> {
        let (conn, screen) = x11rb::connect(Some(display))?;
        let root = conn.setup().roots[screen].root;
        let active = conn.intern_atom(false, b"_NET_ACTIVE_WINDOW")?;
        let name = conn.intern_atom(false, b"_NET_WM_NAME")?;
        let utf8 = conn.intern_atom(false, b"UTF8_STRING")?;
        let pid = conn.intern_atom(false, b"_NET_WM_PID")?;
        let (active, name, utf8, pid) = (
            active.reply()?.atom,
            name.reply()?.atom,
            utf8.reply()?.atom,
            pid.reply()?.atom,
        );
        Ok(Self {
            conn,
            root,
            active,
            name,
            utf8,
            pid,
        })
    }
    fn capture(&self) -> XResult<Option<WindowInfo>> {
        let reply = self
            .conn
            .get_property(false, self.root, self.active, AtomEnum::WINDOW, 0, 1)?
            .reply()?;
        let Some(window) = reply
            .value32()
            .and_then(|mut v| v.next())
            .filter(|w| *w != 0)
        else {
            return Ok(None);
        };
        // Queue all property requests before waiting, minimizing round trips.
        // Property lengths are capped, even for a buggy/malicious client.
        // WM_CLASS is STRING by ICCCM, but some clients set UTF8_STRING: ask for
        // whatever type it has and decode by the type that comes back.
        let class =
            self.conn
                .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::ANY, 0, 1024)?;
        let name = self
            .conn
            .get_property(false, window, self.name, self.utf8, 0, 1024)?;
        let old_name =
            self.conn
                .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)?;
        let pid = self
            .conn
            .get_property(false, window, self.pid, AtomEnum::CARDINAL, 0, 1)?;
        // A destroyed or closing window answers BadWindow: ordinary absence, and
        // the connection stays up. Only a connection-level failure is an error
        // (the caller then reconnects). Missing properties return empty replies
        // and produce partial info.
        let replies = (|| -> Result<_, ReplyError> {
            Ok((
                class.reply()?,
                name.reply()?,
                old_name.reply()?,
                pid.reply()?,
            ))
        })();
        let (class, name, old_name, pid) = match replies {
            Ok(replies) => replies,
            Err(ReplyError::X11Error(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let (instance, class) =
            parse_wm_class(&class.value, class.type_ == u32::from(AtomEnum::STRING));
        let title = text(&name.value).or_else(|| {
            if old_name.type_ == u32::from(AtomEnum::STRING) {
                // ICCCM STRING is ISO-8859-1, whereas _NET_WM_NAME is UTF-8.
                (!old_name.value.is_empty())
                    .then(|| old_name.value.iter().copied().map(char::from).collect())
            } else if old_name.type_ == self.utf8 {
                text(&old_name.value)
            } else {
                None
            }
        });
        let pid = pid
            .value32()
            .and_then(|mut values| values.next())
            .filter(|pid| *pid != 0);
        let process_name = pid
            .and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok())
            .and_then(|s| text(s.trim().as_bytes()));
        Ok(Some(WindowInfo {
            instance,
            class,
            title,
            pid,
            process_name,
        }))
    }
}
/// Split a `WM_CLASS` value (`instance\0class\0`) into its two names.
/// `latin1` is true for the ICCCM `STRING` type (ISO-8859-1); any other type
/// (`UTF8_STRING`) is decoded as UTF-8.
fn parse_wm_class(value: &[u8], latin1: bool) -> (Option<String>, Option<String>) {
    let mut parts = value.split(|b| *b == 0).map(|part| {
        if latin1 {
            (!part.is_empty()).then(|| part.iter().copied().map(char::from).collect())
        } else {
            text(part)
        }
    });
    let instance = parts.next().flatten();
    let class = parts.next().flatten();
    (instance, class)
}
fn text(value: &[u8]) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(
            String::from_utf8_lossy(value)
                .trim_end_matches('\0')
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stalled_provider_cannot_stall_a_session_or_grow_the_queue() {
        let context = X11Context::worker(Arc::default(), || {
            || {
                std::thread::sleep(Duration::from_millis(100));
                None
            }
        });
        for _ in 0..5 {
            let start = Instant::now();
            assert_eq!(context.capture(), None);
            assert!(
                start.elapsed() < Duration::from_millis(60),
                "capture was unbounded"
            );
        }
    }
    #[test]
    fn a_missing_display_returns_none_without_panicking() {
        let context = X11Context::new("invalid-display".into());
        assert_eq!(context.capture(), None);
        assert_eq!(context.capture(), None);
    }
    #[test]
    fn the_worker_prepares_its_connection_before_any_request() {
        // `make` is where `new` connects: it must have run with no capture().
        let ran = Arc::new(AtomicUsize::new(0));
        let flag = ran.clone();
        let _context = X11Context::worker(Arc::default(), move || {
            flag.fetch_add(1, Ordering::SeqCst);
            || None
        });
        let start = Instant::now();
        while ran.load(Ordering::SeqCst) == 0 {
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "the worker did not prepare before the first request"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn wm_class_accepts_string_and_utf8_string_types() {
        // ICCCM STRING is ISO-8859-1.
        assert_eq!(
            parse_wm_class(b"caf\xe9\0Caf\xe9App\0", true),
            (Some("café".into()), Some("CaféApp".into()))
        );
        // UTF8_STRING.
        assert_eq!(
            parse_wm_class("café\0CaféApp\0".as_bytes(), false),
            (Some("café".into()), Some("CaféApp".into()))
        );
        assert_eq!(parse_wm_class(b"", true), (None, None));
        assert_eq!(
            parse_wm_class(b"only\0", false),
            (Some("only".into()), None)
        );
    }
}
