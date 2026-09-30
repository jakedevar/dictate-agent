//! X11 paste handshake. Property data belongs to the requestor, so restoring
//! the selection after SelectionNotify cannot replace text already transferred.
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use arboard::{Clipboard, ImageData};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

pub(crate) enum Snapshot {
    Empty,
    Text(String),
    Html { html: String, text: Option<String> },
    Image(ImageData<'static>),
}

fn optional<T>(value: Result<T, arboard::Error>) -> Result<Option<T>> {
    match value {
        Ok(value) => Ok(Some(value)),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

impl Snapshot {
    pub(crate) fn capture(clipboard: &mut Clipboard) -> Result<Self> {
        // Preserve PNG pixels even when its owner also offers a textual label.
        if let Some(image) = optional(clipboard.get_image()).context("saving clipboard image")? {
            return Ok(Self::Image(image));
        }
        let text = optional(clipboard.get_text()).context("saving clipboard text")?;
        if let Some(html) = optional(clipboard.get().html()).context("saving clipboard HTML")? {
            return Ok(Self::Html { html, text });
        }
        Ok(text.map_or(Self::Empty, Self::Text))
    }

    pub(crate) fn restore(self, clipboard: &mut Clipboard) -> Result<()> {
        match self {
            Self::Empty => clipboard.clear(),
            Self::Text(text) => clipboard.set_text(text),
            Self::Html { html, text } => clipboard.set_html(html, text),
            Self::Image(image) => clipboard.set_image(image),
        }
        .context("restoring clipboard")
    }
}

pub(crate) struct PasteSelection {
    conn: RustConnection,
    window: u32,
    selection: Atom,
    targets: Atom,
    utf8: Atom,
    text_targets: Vec<Atom>,
}

impl PasteSelection {
    pub(crate) fn new(text: &str) -> Result<Self> {
        let (conn, screen) = x11rb::connect(None).context("opening paste selection")?;
        // Normal dictations fit a single X11 property. Oversized transfers use
        // the existing Unicode typing fallback; never truncate a payload.
        if text.len() + 64 > conn.maximum_request_bytes() {
            return Err(anyhow!("dictation exceeds X11 single-property paste limit"));
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
        let utf8 = atom(b"UTF8_STRING")?;
        let text_targets = vec![
            utf8,
            atom(b"text/plain;charset=utf-8")?,
            atom(b"text/plain")?,
        ];
        Ok(Self {
            conn,
            window,
            selection,
            targets,
            utf8,
            text_targets,
        })
    }

    pub(crate) fn claim(&self) -> Result<()> {
        self.conn
            .set_selection_owner(self.window, self.selection, CURRENT_TIME)?
            .check()?;
        if self
            .conn
            .get_selection_owner(self.selection)?
            .reply()?
            .owner
            != self.window
        {
            return Err(anyhow!("could not acquire paste selection"));
        }
        Ok(())
    }

    pub(crate) fn transfer(&self, text: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.conn.poll_for_event()? {
                Some(Event::SelectionRequest(request)) if request.selection == self.selection => {
                    let property = if request.property == NONE {
                        request.target
                    } else {
                        request.property
                    };
                    let is_text = self.text_targets.contains(&request.target);
                    let accepted = if request.target == self.targets {
                        let mut targets = self.text_targets.clone();
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
                    } else if is_text {
                        self.conn
                            .change_property8(
                                PropMode::REPLACE,
                                request.requestor,
                                property,
                                self.utf8,
                                text.as_bytes(),
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
                    if is_text {
                        // The application now has the payload in its own
                        // property; no arbitrary delay or focus change needed.
                        return Ok(());
                    }
                }
                Some(Event::SelectionClear(_)) => {
                    return Err(anyhow!("paste selection ownership lost"))
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }
        Err(anyhow!(
            "application did not consume Ctrl+V clipboard paste within 2s"
        ))
    }
}
