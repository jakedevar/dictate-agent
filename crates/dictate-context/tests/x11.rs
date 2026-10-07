#![cfg(feature = "x11-tests")]
use dictate_context::{ContextProvider, WindowInfo, X11Context};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, CreateWindowAux, PropMode, WindowClass};
use x11rb::wrapper::ConnectionExt as _;

struct Cleanup(Child);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn private_xvfb_captures_properties_missing_focus_and_destroyed_windows_with_latency() {
    let number = (30_000 + std::process::id() % 10_000..50_000)
        .find(|n| {
            !std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
                && !std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists()
        })
        .expect("unused private display number");
    let display = format!(":{number}");
    let child = Command::new("Xvfb")
        .args([&display, "-screen", "0", "800x600x24", "-nolisten", "tcp"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => Cleanup(child),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping live context test: Xvfb is absent");
            return;
        }
        Err(e) => panic!("start private Xvfb: {e}"),
    };
    let start = Instant::now();
    let (conn, screen) = loop {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "private Xvfb exited before readiness"
        );
        if let Ok(connection) = x11rb::connect(Some(&display)) {
            break connection;
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "Xvfb did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let root = conn.setup().roots[screen].root;
    let active = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let name = conn
        .intern_atom(false, b"_NET_WM_NAME")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let utf8 = conn
        .intern_atom(false, b"UTF8_STRING")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let pid = conn
        .intern_atom(false, b"_NET_WM_PID")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let provider = X11Context::new(display);
    // The worker connects eagerly: wait for that (not for a first capture),
    // then the very first capture must already be served by a ready connection.
    let ready = Instant::now();
    while provider.connection_count() == 0 {
        assert!(
            ready.elapsed() < Duration::from_secs(3),
            "the worker never connected on its own"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(provider.capture(), None, "Xvfb has no WM/active property");
    let window = conn.generate_id().unwrap();
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        80,
        80,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap()
    .check()
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"ghostty\0com.mitchellh.ghostty\0",
    )
    .unwrap()
    .check()
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        window,
        name,
        utf8,
        "Synthetic Claude — fixture".as_bytes(),
    )
    .unwrap()
    .check()
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"Legacy caf\xe9 fixture",
    )
    .unwrap()
    .check()
    .unwrap();
    conn.change_property32(
        PropMode::REPLACE,
        window,
        pid,
        AtomEnum::CARDINAL,
        &[std::process::id()],
    )
    .unwrap()
    .check()
    .unwrap();
    conn.change_property32(PropMode::REPLACE, root, active, AtomEnum::WINDOW, &[window])
        .unwrap()
        .check()
        .unwrap();
    let expected = WindowInfo {
        instance: Some("ghostty".into()),
        class: Some("com.mitchellh.ghostty".into()),
        title: Some("Synthetic Claude — fixture".into()),
        pid: Some(std::process::id()),
        process_name: Some(
            std::fs::read_to_string(format!("/proc/{}/comm", std::process::id()))
                .unwrap()
                .trim()
                .into(),
        ),
    };
    assert_eq!(provider.capture(), Some(expected.clone()));
    let mut latencies = Vec::new();
    for _ in 0..500 {
        let start = Instant::now();
        assert_eq!(provider.capture(), Some(expected.clone()));
        latencies.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    latencies.sort_by(f64::total_cmp);
    println!(
        "Xvfb capture n=500 p50={:.3}ms p99={:.3}ms max={:.3}ms",
        latencies[250], latencies[495], latencies[499]
    );
    // The design budget is 8 ms (the provider returns None past it); a loaded
    // CI host may legitimately spike, so only a generous bound is asserted and
    // the measured latency is printed above for the record.
    assert!(
        latencies[495] < 50.0,
        "capture p99 {:.3}ms exceeded the generous 50ms bound",
        latencies[495]
    );
    conn.delete_property(window, name).unwrap().check().unwrap();
    assert_eq!(
        provider.capture().unwrap().title.as_deref(),
        Some("Legacy café fixture")
    );
    conn.delete_property(window, AtomEnum::WM_CLASS.into())
        .unwrap()
        .check()
        .unwrap();
    let partial = provider.capture().unwrap();
    assert_eq!(partial.class, None);
    assert_eq!(partial.instance, None);
    assert_eq!(partial.pid, Some(std::process::id()));
    conn.destroy_window(window).unwrap().check().unwrap();
    assert_eq!(
        provider.capture(),
        None,
        "destroyed active window is ordinary absence"
    );
    conn.change_property32(PropMode::REPLACE, root, active, AtomEnum::WINDOW, &[0])
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(provider.capture(), None, "no focused window");
    // A BadWindow is ordinary absence, not a connection failure: the one
    // eager connection must have served every request above.
    assert_eq!(
        provider.connection_count(),
        1,
        "BadWindow must not reconnect"
    );
    // UTF8_STRING WM_CLASS (some clients set it) yields context too.
    let utf8_window = conn.generate_id().unwrap();
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        utf8_window,
        root,
        0,
        0,
        80,
        80,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap()
    .check()
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        utf8_window,
        AtomEnum::WM_CLASS,
        utf8,
        "café\0Café.App\0".as_bytes(),
    )
    .unwrap()
    .check()
    .unwrap();
    conn.change_property32(
        PropMode::REPLACE,
        root,
        active,
        AtomEnum::WINDOW,
        &[utf8_window],
    )
    .unwrap()
    .check()
    .unwrap();
    let info = provider
        .capture()
        .expect("a UTF8_STRING WM_CLASS is context");
    assert_eq!(info.instance.as_deref(), Some("café"));
    assert_eq!(info.class.as_deref(), Some("Café.App"));
    // The connection is reusable after recovery.
    conn.change_property32(PropMode::REPLACE, root, active, AtomEnum::WINDOW, &[root])
        .unwrap()
        .check()
        .unwrap();
    assert!(provider.capture().is_some());
}
