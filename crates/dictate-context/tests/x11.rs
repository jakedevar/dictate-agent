#![cfg(feature = "x11-tests")]
use dictate_context::{ContextProvider, WindowInfo, X11Context};
use std::io::{BufRead, BufReader};
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
    let child = Command::new("Xvfb")
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
        .spawn();
    let mut child = match child {
        Ok(child) => Cleanup(child),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping live context test: Xvfb is absent");
            return;
        }
        Err(e) => panic!("start private Xvfb: {e}"),
    };
    // Xvfb chooses an unused display and reports only once ready. Bound startup.
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let _ = tx.send(line);
    });
    let number = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("Xvfb ready display number");
    let number: u32 = number.trim().parse().unwrap();
    assert_ne!(number, 0, "test must never use :0");
    let display = format!(":{number}");
    let (conn, screen) = x11rb::connect(Some(&display)).unwrap();
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
        b"Legacy fixture",
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
    assert!(latencies[495] < 10.0, "capture p99 exceeded 10ms");
    conn.delete_property(window, name).unwrap().check().unwrap();
    assert_eq!(
        provider.capture().unwrap().title.as_deref(),
        Some("Legacy fixture")
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
    assert_eq!(provider.capture(), None, "reconnect after BadWindow");
    // The connection is reusable after recovery.
    conn.change_property32(PropMode::REPLACE, root, active, AtomEnum::WINDOW, &[root])
        .unwrap()
        .check()
        .unwrap();
    assert!(provider.capture().is_some());
}
