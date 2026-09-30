use std::sync::mpsc::{sync_channel, SyncSender};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt, Window};
use x11rb::rust_connection::RustConnection;

use crate::{ContextProvider, WindowInfo};

/// A single dedicated worker and a bounded queue. A hung X server can consume
/// at most this one thread, never an async worker or an unbounded blocking pool.
/// No logging on missing focus or display failures: these are normal states.
pub struct X11Context {
    requests: SyncSender<Request>,
}
struct Request {
    deadline: Instant,
    reply: SyncSender<Option<WindowInfo>>,
}
const BUDGET: Duration = Duration::from_millis(8);

impl X11Context {
    /// Explicit display, also used by isolated Xvfb tests. Never changes DISPLAY.
    pub fn new(display: String) -> Self {
        let mut connection = None;
        Self::worker(move || {
            if connection.is_none() {
                connection = X11Connection::connect(&display).ok();
            }
            let result = connection.as_ref()?.capture();
            match result {
                Ok(window) => window,
                Err(_) => {
                    connection = None;
                    None
                } // reconnect on next request
            }
        })
    }
    fn worker(mut capture: impl FnMut() -> Option<WindowInfo> + Send + 'static) -> Self {
        let (requests, rx) = sync_channel::<Request>(1);
        // Failure to create the worker leaves a disconnected sender. capture()
        // then returns None immediately, preserving the no-context path.
        let _ = std::thread::Builder::new()
            .name("dictate-context-x11".into())
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    if Instant::now() >= request.deadline {
                        continue;
                    }
                    let result = capture();
                    let _ = request.reply.try_send(result);
                }
            });
        Self { requests }
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
        let class =
            self.conn
                .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)?;
        let name = self
            .conn
            .get_property(false, window, self.name, self.utf8, 0, 1024)?;
        let old_name =
            self.conn
                .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)?;
        let pid = self
            .conn
            .get_property(false, window, self.pid, AtomEnum::CARDINAL, 0, 1)?;
        // Destroyed windows return BadWindow, causing a quiet reconnect. Missing
        // properties instead return empty replies and produce partial info.
        let (class, name, old_name, pid) = (
            class.reply()?,
            name.reply()?,
            old_name.reply()?,
            pid.reply()?,
        );
        let mut classes = class.value.split(|b| *b == 0);
        let instance = text(classes.next().unwrap_or_default());
        let class = text(classes.next().unwrap_or_default());
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
        let context = X11Context::worker(|| {
            std::thread::sleep(Duration::from_millis(100));
            None
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
}
