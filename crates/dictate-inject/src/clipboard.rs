//! X11 selection transactions. Completion is a heuristic: a text property was
//! delivered to a requestor with the focused window's X11 client resource base.
//! Clipboard managers use another client and cannot acknowledge delivery.
//! Clients using a separate clipboard connection conservatively time out.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    SelectionRequestEvent, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

#[derive(Clone)]
pub(crate) struct Property {
    type_: Atom,
    format: u8,
    data: Vec<u8>,
}

type Contents = BTreeMap<Atom, Property>;

pub(crate) struct PasteSelection {
    conn: RustConnection,
    window: u32,
    selection: Atom,
    targets: Atom,
    incr: Atom,
    contents: Contents,
}

/// Bound the engine's synchronous stop action even if X11 is unresponsive.
/// One reusable worker owns the connection; callers fail closed after 8 ms.
pub fn focused_window() -> Option<u32> {
    use std::sync::{mpsc, OnceLock};
    type Request = (String, mpsc::SyncSender<Option<u32>>);
    static WORKER: OnceLock<mpsc::SyncSender<Request>> = OnceLock::new();
    let display = std::env::var("DISPLAY").ok()?;
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Request>(1);
        std::thread::spawn(move || {
            let mut connection: Option<(String, RustConnection)> = None;
            for (display, reply) in rx {
                if connection
                    .as_ref()
                    .is_none_or(|(current, _)| current != &display)
                {
                    connection = x11rb::connect(Some(&display))
                        .ok()
                        .map(|(conn, _)| (display, conn));
                }
                let focus = connection.as_ref().and_then(|(_, conn)| {
                    let focus = conn.get_input_focus().ok()?.reply().ok()?.focus;
                    (focus > 1).then_some(focus)
                });
                let _ = reply.try_send(focus);
            }
        });
        tx
    });
    let (tx, rx) = mpsc::sync_channel(1);
    worker.try_send((display, tx)).ok()?;
    rx.recv_timeout(Duration::from_millis(8)).ok().flatten()
}

impl PasteSelection {
    pub(crate) fn new(text: &str) -> Result<Self> {
        Self::connect(text, None)
    }

    fn connect(text: &str, display: Option<&str>) -> Result<Self> {
        let (conn, screen) = x11rb::connect(display).context("opening paste selection")?;
        if text.len() + 64 > conn.maximum_request_bytes() {
            bail!("dictation exceeds X11 single-property paste limit");
        }
        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            conn.setup().roots[screen].root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )?
        .check()?;
        let atom =
            |name: &[u8]| -> Result<Atom> { Ok(conn.intern_atom(false, name)?.reply()?.atom) };
        let selection = atom(b"CLIPBOARD")?;
        let targets = atom(b"TARGETS")?;
        let incr = atom(b"INCR")?;
        let utf8 = atom(b"UTF8_STRING")?;
        let mut contents = Contents::new();
        for target in [
            utf8,
            atom(b"text/plain;charset=utf-8")?,
            atom(b"text/plain")?,
        ] {
            contents.insert(
                target,
                Property {
                    type_: target,
                    format: 8,
                    data: text.as_bytes().to_vec(),
                },
            );
        }
        // ICCCM STRING is ISO-8859-1. Do not advertise a lossy conversion.
        if text.chars().all(|c| u32::from(c) <= 255) {
            contents.insert(
                AtomEnum::STRING.into(),
                Property {
                    type_: AtomEnum::STRING.into(),
                    format: 8,
                    data: text.chars().map(|c| c as u8).collect(),
                },
            );
        }
        contents.insert(
            atom(b"TEXT")?,
            Property {
                type_: utf8,
                format: 8,
                data: text.as_bytes().to_vec(),
            },
        );
        Ok(Self {
            conn,
            window,
            selection,
            targets,
            incr,
            contents,
        })
    }

    pub(crate) fn owner(&self) -> Result<u32> {
        Ok(self
            .conn
            .get_selection_owner(self.selection)?
            .reply()?
            .owner)
    }

    fn read_target(&self, target: Atom) -> Result<Option<Property>> {
        let property = self
            .conn
            .intern_atom(false, b"DICTATE_SNAPSHOT")?
            .reply()?
            .atom;
        self.conn.delete_property(self.window, property)?.check()?;
        self.conn
            .convert_selection(self.window, self.selection, target, property, CURRENT_TIME)?
            .check()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if Instant::now() >= deadline {
                bail!("clipboard snapshot timed out");
            }
            match self.conn.poll_for_event()? {
                Some(Event::SelectionNotify(event))
                    if event.requestor == self.window && event.target == target =>
                {
                    if event.property == NONE {
                        // Some owners advertise aliases they cannot serve.
                        // There is no payload to preserve for a refused target.
                        return Ok(None);
                    }
                    let reply = self
                        .conn
                        .get_property(true, self.window, property, AtomEnum::ANY, 0, u32::MAX)?
                        .reply()?;
                    // INCR/MULTIPLE/TIMESTAMP are protocols, not reusable data.
                    // Fail before claiming or sending keys if a payload cannot
                    // be preserved in a single property; never silently drop it.
                    if reply.type_ == self.incr
                        || reply.bytes_after != 0
                        || !matches!(reply.format, 8 | 16 | 32)
                    {
                        bail!("clipboard target requires unsupported incremental transfer");
                    }
                    if reply.value.len() + 64 > self.conn.maximum_request_bytes() {
                        bail!("clipboard target exceeds restore limit");
                    }
                    return Ok(Some(Property {
                        type_: reply.type_,
                        format: reply.format,
                        data: reply.value,
                    }));
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Result<(u32, Contents)> {
        let owner = self.owner()?;
        if owner == NONE {
            return Ok((owner, Contents::new()));
        }
        let advertised = self
            .read_target(self.targets)?
            .ok_or_else(|| anyhow!("clipboard TARGETS refused"))?;
        if advertised.type_ != u32::from(AtomEnum::ATOM) || advertised.format != 32 {
            bail!("invalid clipboard TARGETS");
        }
        let mut saved = Contents::new();
        for bytes in advertised.data.chunks_exact(4) {
            let target = u32::from_ne_bytes(bytes.try_into()?);
            let name = self.conn.get_atom_name(target)?.reply()?.name;
            if matches!(
                name.as_slice(),
                b"TARGETS"
                    | b"MULTIPLE"
                    | b"TIMESTAMP"
                    | b"SAVE_TARGETS"
                    | b"DELETE"
                    | b"INSERT_SELECTION"
                    | b"INSERT_PROPERTY"
            ) {
                continue;
            }
            if let Some(value) = self.read_target(target)? {
                saved.insert(target, value);
            }
        }
        if self.owner()? != owner {
            bail!("clipboard changed during snapshot");
        }
        Ok((owner, saved))
    }

    /// Check the snapshot's owner and claim atomically relative to other X11
    /// clients. No other connection is used while the server is grabbed.
    pub(crate) fn claim(&self, prior_owner: u32) -> Result<()> {
        self.conn.grab_server()?.check()?;
        let result = (|| {
            if self.owner()? != prior_owner {
                bail!("clipboard changed before paste");
            }
            self.conn
                .set_selection_owner(self.window, self.selection, CURRENT_TIME)?
                .check()?;
            Ok(())
        })();
        self.conn.ungrab_server()?.check()?;
        result
    }

    pub(crate) fn focus(&self) -> Result<Option<u32>> {
        let focus = self.conn.get_input_focus()?.reply()?.focus;
        Ok((focus > 1).then_some(focus))
    }

    /// Send Ctrl+V on the same connection under the server grab, so a
    /// queued focus change cannot land between validation and key delivery.
    pub(crate) fn send_paste(&self, destination: u32) -> Result<bool> {
        use x11rb::protocol::xproto::{KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
        use x11rb::protocol::xtest::ConnectionExt as _;
        let setup = self.conn.setup();
        let mapping = self
            .conn
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)?
            .reply()?;
        let keycode = |keysym| -> Result<u8> {
            mapping
                .keysyms
                .chunks(usize::from(mapping.keysyms_per_keycode))
                .position(|syms| syms.contains(&keysym))
                .map(|index| setup.min_keycode + index as u8)
                .ok_or_else(|| anyhow!("paste key unavailable"))
        };
        let ctrl = keycode(0xffe3)?;
        let v = keycode(0x76)?;
        self.conn.grab_server()?.check()?;
        let result = (|| {
            if self.focus()? != Some(destination) {
                return Ok(false);
            }
            self.conn
                .xtest_fake_input(KEY_PRESS_EVENT, ctrl, CURRENT_TIME, NONE, 0, 0, 0)?
                .check()?;
            let click = (|| -> Result<()> {
                self.conn
                    .xtest_fake_input(KEY_PRESS_EVENT, v, CURRENT_TIME, NONE, 0, 0, 0)?
                    .check()?;
                self.conn
                    .xtest_fake_input(KEY_RELEASE_EVENT, v, CURRENT_TIME, NONE, 0, 0, 0)?
                    .check()?;
                Ok(())
            })();
            let release = self
                .conn
                .xtest_fake_input(KEY_RELEASE_EVENT, ctrl, CURRENT_TIME, NONE, 0, 0, 0)?
                .check();
            click?;
            release?;
            Ok(true)
        })();
        self.conn.ungrab_server()?.check()?;
        result
    }

    fn serve(&self, request: SelectionRequestEvent) -> Result<bool> {
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let accepted = if request.target == self.targets {
            let mut targets: Vec<_> = self.contents.keys().copied().collect();
            targets.push(self.targets);
            self.conn
                .change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )?
                .check()?;
            true
        } else if let Some(value) = self.contents.get(&request.target) {
            self.conn
                .change_property(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    value.type_,
                    value.format,
                    (value.data.len() / usize::from(value.format / 8)) as u32,
                    &value.data,
                )?
                .check()?;
            true
        } else {
            false
        };
        self.conn
            .send_event(
                false,
                request.requestor,
                EventMask::NO_EVENT,
                SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: request.time,
                    requestor: request.requestor,
                    selection: request.selection,
                    target: request.target,
                    property: if accepted { property } else { NONE },
                },
            )?
            .check()?;
        Ok(accepted && request.target != self.targets)
    }

    pub(crate) fn transfer(&self, destination: u32) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mask = !self.conn.setup().resource_id_mask;
        while Instant::now() < deadline {
            match self.conn.poll_for_event()? {
                Some(Event::SelectionRequest(request)) if request.selection == self.selection => {
                    let delivered = self.serve(request)?;
                    if delivered && request.requestor & mask == destination & mask {
                        return Ok(());
                    }
                }
                Some(Event::SelectionClear(_)) => bail!("paste selection ownership lost"),
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }
        Err(anyhow!(
            "paste unconfirmed; dictation remains on clipboard if still owned; no typing retry"
        ))
    }

    /// Restore by changing the served contents, not by claiming a new owner.
    /// A concurrent copy wins automatically. Empty selections are relinquished
    /// only under a server grab with an ownership check.
    pub(crate) fn restore(&mut self, saved: Contents) -> Result<()> {
        self.conn.grab_server()?.check()?;
        let result = (|| {
            if self.owner()? == self.window {
                self.contents = saved;
                if self.contents.is_empty() {
                    self.conn
                        .set_selection_owner(NONE, self.selection, CURRENT_TIME)?
                        .check()?;
                }
            }
            Ok(())
        })();
        self.conn.ungrab_server()?.check()?;
        result
    }

    /// Keep the current payload alive for delayed requests and future manual
    /// pastes. The server sends SelectionClear on the next copy/transaction.
    pub(crate) fn retain(self) {
        std::thread::spawn(move || {
            while let Ok(event) = self.conn.wait_for_event() {
                match event {
                    Event::SelectionRequest(request) if request.selection == self.selection => {
                        let _ = self.serve(request);
                    }
                    Event::SelectionClear(_) => break,
                    _ => {}
                }
            }
        });
    }
}

#[cfg(all(test, feature = "x11-tests"))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;

    struct Server {
        child: Child,
        display: String,
    }
    impl Server {
        fn start() -> Self {
            let mut child = Command::new("Xvfb")
                .args([
                    "-displayfd",
                    "1",
                    "-screen",
                    "0",
                    "800x600x24",
                    "-nolisten",
                    "tcp",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("private Xvfb required");
            let mut number = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut number)
                .unwrap();
            assert!(
                !number.trim().is_empty(),
                "Xvfb did not allocate a private display"
            );
            Self {
                child,
                display: format!(":{}", number.trim()),
            }
        }
        fn client(&self) -> (Arc<RustConnection>, u32) {
            let (conn, screen) = x11rb::connect(Some(&self.display)).unwrap();
            let window = conn.generate_id().unwrap();
            conn.create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                conn.setup().roots[screen].root,
                0,
                0,
                20,
                20,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new(),
            )
            .unwrap()
            .check()
            .unwrap();
            (Arc::new(conn), window)
        }
        fn paste(&self, text: &str) -> PasteSelection {
            PasteSelection::connect(text, Some(&self.display)).unwrap()
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    fn atom(conn: &RustConnection, name: &[u8]) -> Atom {
        conn.intern_atom(false, name).unwrap().reply().unwrap().atom
    }
    fn request(conn: &RustConnection, window: u32, target: Atom, legacy: bool) -> Property {
        let property = if legacy {
            NONE
        } else {
            atom(conn, b"TEST_RESULT")
        };
        conn.convert_selection(
            window,
            atom(conn, b"CLIPBOARD"),
            target,
            property,
            CURRENT_TIME,
        )
        .unwrap()
        .check()
        .unwrap();
        loop {
            if let Event::SelectionNotify(event) = conn.wait_for_event().unwrap() {
                assert_ne!(event.property, NONE, "target accepted");
                let reply = conn
                    .get_property(true, window, event.property, AtomEnum::ANY, 0, u32::MAX)
                    .unwrap()
                    .reply()
                    .unwrap();
                return Property {
                    type_: reply.type_,
                    format: reply.format,
                    data: reply.value,
                };
            }
        }
    }
    fn focus(conn: &RustConnection, window: u32) {
        use x11rb::protocol::xproto::InputFocus;
        conn.map_window(window).unwrap().check().unwrap();
        conn.set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
    }

    #[test]
    fn clipboard_manager_cannot_acknowledge_destination_consumption() {
        let server = Server::start();
        let (destination, dest_window) = server.client();
        let (manager, manager_window) = server.client();
        let paste = server.paste("synthetic dictation");
        paste.claim(NONE).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let transfer = std::thread::spawn(move || {
            let result = paste.transfer(dest_window);
            done_tx.send(result.is_ok()).unwrap();
            (paste, result)
        });
        let utf8 = atom(&manager, b"UTF8_STRING");
        assert_eq!(
            request(&manager, manager_window, utf8, false).data,
            b"synthetic dictation"
        );
        assert!(
            done_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "manager must not finish paste"
        );
        // A separate requestor window on the destination's client is valid.
        assert_eq!(
            request(&destination, dest_window, utf8, false).data,
            b"synthetic dictation"
        );
        assert!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(transfer.join().unwrap().1.is_ok());
    }

    #[test]
    fn concurrent_copy_wins_over_restore_and_snapshot_claim() {
        let server = Server::start();
        let (other, other_window) = server.client();
        let mut paste = server.paste("dictation");
        paste.claim(NONE).unwrap();
        other
            .set_selection_owner(other_window, paste.selection, CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
        assert!(
            paste.transfer(other_window).is_err(),
            "SelectionClear ends an unconfirmed transfer"
        );
        paste.restore(Contents::new()).unwrap();
        assert_eq!(
            paste.owner().unwrap(),
            other_window,
            "restore must not overwrite concurrent copy"
        );
        assert!(
            paste.claim(NONE).is_err(),
            "snapshot-to-claim race must not overwrite concurrent copy"
        );
        assert_eq!(paste.owner().unwrap(), other_window);
    }

    #[test]
    fn timed_out_paste_keeps_dictation_for_late_request() {
        let server = Server::start();
        let (destination, dest_window) = server.client();
        let paste = server.paste("late dictation");
        paste.claim(NONE).unwrap();
        assert!(paste.transfer(dest_window).is_err());
        // No restore and no key retry: the retained selection serves the same
        // dictation to a request arriving after the timeout.
        paste.retain();
        let utf8 = atom(&destination, b"UTF8_STRING");
        assert_eq!(
            request(&destination, dest_window, utf8, false).data,
            b"late dictation"
        );
    }

    #[test]
    fn target_types_legacy_property_and_raw_snapshot_are_preserved() {
        let server = Server::start();
        let (client, window) = server.client();
        let mut paste = server.paste("caf\u{e9}");
        let utf8 = atom(&client, b"UTF8_STRING");
        let text = atom(&client, b"TEXT");
        let string = u32::from(AtomEnum::STRING);
        let raw = atom(&client, b"application/x-synthetic");
        paste.contents.insert(
            raw,
            Property {
                type_: raw,
                format: 16,
                data: vec![1, 2, 3, 4],
            },
        );
        let files = atom(&client, b"text/uri-list");
        paste.contents.insert(
            files,
            Property {
                type_: files,
                format: 8,
                data: b"file:///tmp/synthetic-file.txt\r\n".to_vec(),
            },
        );
        paste.claim(NONE).unwrap();
        paste.retain();
        let advertised = request(&client, window, atom(&client, b"TARGETS"), false);
        assert_eq!(
            (advertised.type_, advertised.format),
            (AtomEnum::ATOM.into(), 32)
        );
        let targets: Vec<_> = advertised
            .data
            .chunks_exact(4)
            .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
            .collect();
        assert!(targets.contains(&string) && targets.contains(&text) && targets.contains(&utf8));
        let latin = request(&client, window, string, true);
        assert_eq!(
            (latin.type_, latin.format, latin.data),
            (string, 8, b"caf\xe9".to_vec())
        );
        let value = request(&client, window, text, false);
        assert_eq!(
            (value.type_, value.format, value.data),
            (utf8, 8, "caf\u{e9}".as_bytes().to_vec())
        );
        let next = server.paste("replacement");
        let (owner, saved) = next.snapshot().unwrap();
        assert_eq!(
            (
                saved[&raw].type_,
                saved[&raw].format,
                saved[&raw].data.clone()
            ),
            (raw, 16, vec![1, 2, 3, 4])
        );
        next.claim(owner).unwrap();
        let mut next = next;
        next.restore(saved).unwrap();
        next.retain();
        assert_eq!(request(&client, window, raw, false).data, vec![1, 2, 3, 4]);
        assert_eq!(
            request(&client, window, files, false).data,
            b"file:///tmp/synthetic-file.txt\r\n"
        );
    }

    #[test]
    fn incr_and_oversized_payloads_fail_before_claiming_or_sending_keys() {
        let server = Server::start();
        let (client, _) = server.client();
        let mut original = server.paste("original");
        let raw = atom(&client, b"application/x-large-synthetic");
        original.contents.insert(
            raw,
            Property {
                type_: original.incr,
                format: 32,
                data: 1_000_000u32.to_ne_bytes().to_vec(),
            },
        );
        original.claim(NONE).unwrap();
        let original_owner = original.window;
        original.retain();
        let replacement = server.paste("replacement");
        assert!(
            replacement.snapshot().is_err(),
            "INCR cannot be snapshotted as ordinary bytes"
        );
        assert_eq!(
            replacement.owner().unwrap(),
            original_owner,
            "old clipboard remains owned"
        );
        let oversized = "x".repeat(client.maximum_request_bytes());
        assert!(PasteSelection::connect(&oversized, Some(&server.display)).is_err());
        assert_eq!(replacement.owner().unwrap(), original_owner);
    }

    #[test]
    fn latin1_string_is_not_advertised_for_unrepresentable_unicode() {
        let server = Server::start();
        let (client, window) = server.client();
        let paste = server.paste("synthetic \u{03bb}");
        paste.claim(NONE).unwrap();
        paste.retain();
        let advertised = request(&client, window, atom(&client, b"TARGETS"), false);
        let targets: Vec<_> = advertised
            .data
            .chunks_exact(4)
            .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
            .collect();
        assert!(!targets.contains(&u32::from(AtomEnum::STRING)));
        assert_eq!(
            request(&client, window, atom(&client, b"TEXT"), false).data,
            "synthetic \u{03bb}".as_bytes()
        );
    }

    #[test]
    fn changed_focus_sends_no_paste_keys() {
        let server = Server::start();
        let (client, first) = server.client();
        let (other, second) = server.client();
        focus(&client, first);
        let paste = server.paste("focus dictation");
        paste.claim(NONE).unwrap();
        focus(&other, second);
        assert!(!paste.send_paste(first).unwrap());
        paste.retain();
        assert_eq!(
            request(&other, second, atom(&other, b"UTF8_STRING"), false).data,
            b"focus dictation"
        );
    }
}
