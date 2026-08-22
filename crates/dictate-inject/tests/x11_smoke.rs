//! Real X11 delivery smoke. It starts its own X server so CI does not need a
//! desktop session; when Xvfb/xterm/xdotool are absent it intentionally skips.

use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use dictate_inject::{InjectionPolicy, Injector, OutputConfig, X11Injector};
use dictate_proto::InjectionOutcome;

struct ChildCleanup {
    xterm: std::process::Child,
    xvfb: std::process::Child,
    socket: std::path::PathBuf,
}

impl Drop for ChildCleanup {
    fn drop(&mut self) {
        let _ = self.xterm.kill();
        let _ = self.xterm.wait();
        let _ = self.xvfb.kill();
        let _ = self.xvfb.wait();
        let _ = fs::remove_file(&self.socket);
    }
}

fn has_tool(name: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name} >/dev/null"))
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn xvfb_xterm_reads_back_real_direct_injection() {
    if !["Xvfb", "xterm", "xdotool", "xdpyinfo"]
        .into_iter()
        .all(has_tool)
    {
        eprintln!("skipping X11 injection smoke: Xvfb, xterm, xdotool, or xdpyinfo unavailable");
        return;
    }
    let display_number = 20_000 + std::process::id() % 10_000;
    let display = format!(":{display_number}");
    let socket = format!("/tmp/.X11-unix/X{display_number}");
    if std::path::Path::new(&socket).exists() {
        eprintln!("skipping X11 injection smoke: {display} is already in use");
        return;
    }
    let stamp = format!("dictate-inject-smoke-{}", std::process::id());
    let output = std::env::temp_dir().join(&stamp);
    let xvfb = Command::new("Xvfb")
        .args([&display, "-screen", "0", "800x600x24", "-nolisten", "tcp"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start Xvfb");
    let started = Instant::now();
    while !Command::new("xdpyinfo")
        .env("DISPLAY", &display)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
        && started.elapsed() < Duration::from_secs(3)
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        Command::new("xdpyinfo")
            .env("DISPLAY", &display)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success()),
        "Xvfb did not become ready"
    );

    let script = "IFS= read -r -n 11 line; printf '%s' \"$line\" > \"$1\"";
    let xterm = Command::new("xterm")
        .env("DISPLAY", &display)
        .args([
            "-title",
            &stamp,
            "-e",
            "bash",
            "-c",
            script,
            "_",
            output.to_str().expect("utf-8 temp path"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start xterm");
    let _children = ChildCleanup {
        xterm,
        xvfb,
        socket: socket.into(),
    };
    let mut window = None;
    for _ in 0..60 {
        let result = Command::new("xdotool")
            .env("DISPLAY", &display)
            .args(["search", "--name", &stamp])
            .output()
            .ok();
        if let Some(result) = result {
            if let Some(id) = String::from_utf8_lossy(&result.stdout)
                .lines()
                .next()
                .filter(|id| !id.is_empty())
            {
                window = Some(id.to_owned());
                break;
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    let window = window.expect("xterm window should appear");
    assert!(Command::new("xdotool")
        .env("DISPLAY", &display)
        .args(["windowfocus", "--sync", &window])
        .status()
        .expect("focus xterm")
        .success());

    let prior_display = std::env::var_os("DISPLAY");
    std::env::set_var("DISPLAY", &display);
    let injector = X11Injector::new(&OutputConfig::default());
    // Direct typing is the universally testable X11 path and is also the
    // fallback selected when clipboard save/restore is unavailable.
    let outcome = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(injector.inject("wispr smoke", InjectionPolicy::Type));
    match outcome {
        InjectionOutcome::Injected { .. } => {}
        unexpected => panic!("expected an injection, got {unexpected:?}"),
    }
    match prior_display {
        Some(value) => std::env::set_var("DISPLAY", value),
        None => std::env::remove_var("DISPLAY"),
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    while !output.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    let read_back = fs::read_to_string(&output).expect("xterm should receive pasted text");
    let _ = fs::remove_file(&output);
    assert_eq!(read_back, "wispr smoke");
}
