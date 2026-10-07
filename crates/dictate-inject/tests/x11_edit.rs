//! A tiny synthetic text widget in a private Xvfb. Never reads or types into
//! the real desktop; no real user content or application is needed.
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arboard::Clipboard;
use dictate_inject::selection::focused_window;
use dictate_inject::{OutputConfig, X11Injector};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt, CreateWindowAux, EventMask, InputFocus, KeyButMask, PropMode,
    SelectionNotifyEvent, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

struct DisplayGuard {
    server: Child,
}
impl Drop for DisplayGuard {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}
#[derive(Default)]
struct Widget {
    selected: String,
    document: String,
    pastes: usize,
    primary: String,
    ignore_paste: bool,
    paste_requests: usize,
    late_request: bool,
    rejected_pastes: usize,
}

fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "synthetic widget timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn private_xvfb_selection_roundtrip_and_failures_preserve_document_clipboard_and_focus() {
    if Command::new("Xvfb")
        .arg("-help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping selection roundtrip: Xvfb unavailable");
        return;
    }
    if std::env::var_os("DICTATE_PRIVATE_X11_EDIT").is_some() {
        run_edit();
        return;
    }
    // Xvfb allocates an unused display instead of guessing an existing one.
    let path = format!("/tmp/s25-display-{}", std::process::id());
    let display_file = std::fs::File::create(&path).unwrap();
    // Child stdio fd 1 is the private display-number pipe/file.
    let server = Command::new("Xvfb")
        .args([
            "-displayfd",
            "1",
            "-screen",
            "0",
            "800x600x24",
            "-nolisten",
            "tcp",
        ])
        .stdout(Stdio::from(display_file.try_clone().unwrap()))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _display_guard = DisplayGuard { server };
    wait_for(|| std::fs::read_to_string(&path).is_ok_and(|s| !s.trim().is_empty()));
    let number = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "private_xvfb_selection_roundtrip_and_failures_preserve_document_clipboard_and_focus",
            "--nocapture"
        ])
        .env("DISPLAY", format!(":{}", number.trim()))
        .env("DICTATE_PRIVATE_X11_EDIT", "1")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("XAUTHORITY")
        .status()
        .unwrap()
        .success());
}

fn run_edit() {
    let (conn, screen) = x11rb::connect(None).unwrap();
    let atom = |s: &[u8]| conn.intern_atom(false, s).unwrap().reply().unwrap().atom;
    let primary = AtomEnum::PRIMARY.into();
    let clipboard_atom = atom(b"CLIPBOARD");
    let utf8 = atom(b"UTF8_STRING");
    let targets = atom(b"TARGETS");
    let property = atom(b"S25_REPLACEMENT");
    let window = conn.generate_id().unwrap();
    let other = conn.generate_id().unwrap();
    for id in [window, other] {
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            id,
            conn.setup().roots[screen].root,
            0,
            0,
            400,
            100,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS),
        )
        .unwrap()
        .check()
        .unwrap();
        conn.map_window(id).unwrap().check().unwrap();
    }
    conn.set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)
        .unwrap()
        .check()
        .unwrap();
    conn.set_selection_owner(window, primary, CURRENT_TIME)
        .unwrap()
        .check()
        .unwrap();
    let min = conn.setup().min_keycode;
    let mapping = conn
        .get_keyboard_mapping(min, conn.setup().max_keycode - min + 1)
        .unwrap()
        .reply()
        .unwrap();
    let key = |symbol| {
        mapping
            .keysyms
            .chunks(mapping.keysyms_per_keycode as usize)
            .position(|chunk| chunk.contains(&symbol))
            .unwrap() as u8
            + min
    };
    let copy_key = key('c' as u32);
    let paste_key = key('v' as u32);
    let state = Arc::new(Mutex::new(Widget {
        selected: "send the synthetic report".into(),
        primary: "stale text from another widget".into(),
        document: "before |send the synthetic report| after".into(),
        pastes: 0,
        ignore_paste: false,
        paste_requests: 0,
        late_request: false,
        rejected_pastes: 0,
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let thread_state = state.clone();
    let thread_stop = stop.clone();
    let worker = std::thread::spawn(move || {
        while !thread_stop.load(Ordering::Acquire) {
            if std::mem::take(&mut thread_state.lock().unwrap().late_request) {
                conn.convert_selection(window, clipboard_atom, utf8, property, CURRENT_TIME)
                    .unwrap()
                    .check()
                    .unwrap();
            }
            match conn.poll_for_event().unwrap() {
                Some(Event::KeyPress(e))
                    if e.event == window && e.state.contains(KeyButMask::CONTROL) =>
                {
                    if e.detail == copy_key && !thread_state.lock().unwrap().selected.is_empty() {
                        conn.set_selection_owner(window, clipboard_atom, CURRENT_TIME)
                            .unwrap()
                            .check()
                            .unwrap();
                    } else if e.detail == paste_key {
                        {
                            let mut state = thread_state.lock().unwrap();
                            state.paste_requests += 1;
                            if state.ignore_paste {
                                continue;
                            }
                        }
                        conn.convert_selection(
                            window,
                            clipboard_atom,
                            utf8,
                            property,
                            CURRENT_TIME,
                        )
                        .unwrap()
                        .check()
                        .unwrap();
                    }
                }
                Some(Event::SelectionRequest(e)) => {
                    let prop = if e.property == NONE {
                        e.target
                    } else {
                        e.property
                    };
                    let accepted = if e.target == targets {
                        conn.change_property32(
                            PropMode::REPLACE,
                            e.requestor,
                            prop,
                            AtomEnum::ATOM,
                            &[targets, utf8],
                        )
                        .unwrap()
                        .check()
                        .unwrap();
                        true
                    } else if e.target == utf8 {
                        let state = thread_state.lock().unwrap();
                        let text = if e.selection == primary {
                            &state.primary
                        } else {
                            &state.selected
                        };
                        conn.change_property8(
                            PropMode::REPLACE,
                            e.requestor,
                            prop,
                            utf8,
                            text.as_bytes(),
                        )
                        .unwrap()
                        .check()
                        .unwrap();
                        true
                    } else {
                        false
                    };
                    conn.send_event(
                        false,
                        e.requestor,
                        EventMask::NO_EVENT,
                        SelectionNotifyEvent {
                            response_type: SELECTION_NOTIFY_EVENT,
                            sequence: 0,
                            time: e.time,
                            requestor: e.requestor,
                            selection: e.selection,
                            target: e.target,
                            property: if accepted { prop } else { NONE },
                        },
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                }
                Some(Event::SelectionNotify(e)) if e.property == NONE => {
                    thread_state.lock().unwrap().rejected_pastes += 1;
                }
                Some(Event::SelectionNotify(e)) if e.property == property => {
                    let reply = conn
                        .get_property(true, window, property, utf8, 0, u32::MAX)
                        .unwrap()
                        .reply()
                        .unwrap();
                    let replacement = String::from_utf8(reply.value).unwrap();
                    let mut state = thread_state.lock().unwrap();
                    assert!(!state.selected.is_empty(), "paste without selection");
                    state.document = format!("before |{replacement}| after");
                    state.selected.clear();
                    state.pastes += 1;
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    });
    let mut clipboard = Clipboard::new().unwrap();
    clipboard
        .set_text("original synthetic clipboard α")
        .unwrap();
    let injector = X11Injector::new(&OutputConfig::default());
    assert!(
        injector.capture_selection_blocking(window).is_err(),
        "conflicting PRIMARY must not become a target"
    );
    assert_eq!(
        clipboard.get_text().unwrap(),
        "original synthetic clipboard α"
    );
    state.lock().unwrap().primary = "send the synthetic report".into();
    let selected = injector.capture_selection_blocking(window).unwrap();
    assert_eq!(
        selected.text, "send the synthetic report",
        "stale PRIMARY must not win over fresh widget copy"
    );
    assert_eq!(
        clipboard.get_text().unwrap(),
        "original synthetic clipboard α"
    );
    assert!(injector
        .replace_selection_blocking(&selected, "Please send the synthetic report.")
        .did_inject());
    wait_for(|| state.lock().unwrap().pastes == 1);
    assert_eq!(
        state.lock().unwrap().document,
        "before |Please send the synthetic report.| after"
    );
    assert_eq!(
        clipboard.get_text().unwrap(),
        "original synthetic clipboard α"
    );
    assert_eq!(
        focused_window().unwrap(),
        window,
        "never raise or change focus"
    );

    // No selection: never reuse the old PRIMARY or old clipboard contents.
    assert!(injector.capture_selection_blocking(window).is_err());
    assert_eq!(
        clipboard.get_text().unwrap(),
        "original synthetic clipboard α"
    );
    assert_eq!(state.lock().unwrap().pastes, 1);

    // Changed selection: do not replace it with the old request's response.
    state.lock().unwrap().selected = "new selection".into();
    state.lock().unwrap().primary = "new selection".into();
    assert!(!injector
        .replace_selection_blocking(&selected, "obsolete rewrite")
        .did_inject());
    assert_eq!(
        clipboard.get_text().unwrap(),
        "original synthetic clipboard α"
    );
    assert_eq!(state.lock().unwrap().selected, "new selection");
    assert_eq!(state.lock().unwrap().pastes, 1);

    // Focus moved: never activate the original destination or inject elsewhere.
    let (control, _) = x11rb::connect(None).unwrap();
    control
        .set_input_focus(InputFocus::PARENT, other, CURRENT_TIME)
        .unwrap()
        .check()
        .unwrap();
    assert!(!injector
        .replace_selection_blocking(&selected, "obsolete rewrite")
        .did_inject());
    assert_eq!(focused_window().unwrap(), other);
    assert_eq!(
        clipboard.get_text().unwrap(),
        "original synthetic clipboard α"
    );
    assert_eq!(state.lock().unwrap().pastes, 1);
    // A widget that refuses to consume the paste must never trigger retry or
    // direct typing. Preserve the selected text and clipboard on timeout.
    control
        .set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)
        .unwrap()
        .check()
        .unwrap();
    control
        .set_selection_owner(NONE, primary, CURRENT_TIME)
        .unwrap()
        .check()
        .unwrap();
    let fresh = injector.capture_selection_blocking(window).unwrap();
    assert_eq!(
        fresh.text, "new selection",
        "Ctrl+C fallback works without PRIMARY"
    );
    assert_eq!(
        clipboard.get_text().unwrap(),
        "original synthetic clipboard α"
    );
    state.lock().unwrap().ignore_paste = true;
    let document = state.lock().unwrap().document.clone();
    assert!(!injector
        .replace_selection_blocking(&fresh, "unconsumed replacement")
        .did_inject());
    assert_eq!(state.lock().unwrap().document, document);
    assert_eq!(state.lock().unwrap().selected, "new selection");
    assert_eq!(state.lock().unwrap().pastes, 1);
    assert_eq!(
        state.lock().unwrap().paste_requests,
        2,
        "one attempt per edit; no retry"
    );
    assert_eq!(
        clipboard.get_text().unwrap(),
        "original synthetic clipboard α"
    );
    assert_eq!(focused_window().unwrap(), window);
    // A request queued after the unconfirmed Ctrl+V must receive no payload,
    // rather than replacing the selection with the restored old clipboard.
    state.lock().unwrap().late_request = true;
    wait_for(|| state.lock().unwrap().rejected_pastes == 1);
    assert_eq!(state.lock().unwrap().document, document);
    assert_eq!(state.lock().unwrap().selected, "new selection");
    assert_eq!(state.lock().unwrap().pastes, 1);
    assert_eq!(
        clipboard.get_text().unwrap(),
        "original synthetic clipboard α"
    );

    stop.store(true, Ordering::Release);
    worker.join().unwrap();
}
