//! Real X11 delivery smoke. It starts its own X server so CI does not need a
//! desktop session; when Xvfb/xterm/xdotool are absent it intentionally skips.

use arboard::Clipboard;
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use dictate_inject::{InjectionPolicy, OutputConfig, X11Injector};
use dictate_proto::{InjectMethod, InjectionOutcome};

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
fn xvfb_xterm_pastes_and_restores_empty_text_html_and_png_clipboards() {
    if !["Xvfb", "xterm", "xdotool", "xdpyinfo", "xclip"]
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

    // No injection or window discovery ever touches the user's display.
    let prior_display = std::env::var_os("DISPLAY");
    std::env::set_var("DISPLAY", &display);
    let mut clipboard = Clipboard::new().unwrap();
    let injector = X11Injector::new(&OutputConfig::default());
    let mut children = ChildCleanup {
        xterm: Command::new("true").spawn().unwrap(),
        xvfb,
        socket: socket.into(),
    };
    for case in [
        "empty", "text", "png", "html", "long", "delayed", "direct", "fallback",
    ] {
        match case {
            "empty" => clipboard.clear().unwrap(),
            "png" => {
                let mut owner = Command::new("xclip")
                    .env("DISPLAY", &display)
                    .args(["-selection", "clipboard", "-t", "image/png", "-i"])
                    .stdin(Stdio::piped())
                    .spawn()
                    .unwrap();
                owner
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(include_bytes!("fixtures/clipboard.png"))
                    .unwrap();
                assert!(owner.wait().unwrap().success());
            }
            "html" => clipboard
                .set_html("<b>Synthetic prior</b>", Some("Synthetic prior"))
                .unwrap(),
            _ => clipboard.set_text("Synthetic prior λ\nclipboard").unwrap(),
        }
        let prior_image = (case == "png").then(|| clipboard.get_image().unwrap());
        let text = if case == "long" {
            "synthetic ".repeat(1500).trim().to_owned()
        } else {
            "wispr smoke".to_owned()
        };
        let output = std::env::temp_dir().join(format!("{stamp}-{case}"));
        let _ = fs::remove_file(&output);
        let script = "stty -echo; IFS= read -r -n \"$2\" line; printf '%s' \"$line\" > \"$1\"";
        children.xterm = Command::new("xterm")
            .env("DISPLAY", &display)
            .args([
                "-title",
                &stamp,
                "-xrm",
                if case == "fallback" {
                    "XTerm*VT100.translations: #override Ctrl<Key>v: ignore()"
                } else {
                    "XTerm*VT100.translations: #override Ctrl<Key>v: insert-selection(CLIPBOARD)"
                },
                "-e",
                "bash",
                "-c",
                script,
                "_",
                output.to_str().unwrap(),
                &text.len().to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut window = None;
        for _ in 0..120 {
            let result = Command::new("xdotool")
                .env("DISPLAY", &display)
                .args(["search", "--name", &stamp])
                .output()
                .unwrap();
            if let Some(id) = String::from_utf8_lossy(&result.stdout)
                .lines()
                .next()
                .filter(|id| !id.is_empty())
            {
                window = Some(id.to_owned());
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(Command::new("xdotool")
            .env("DISPLAY", &display)
            .args(["windowfocus", "--sync", &window.expect("xterm window")])
            .status()
            .unwrap()
            .success());
        // Wait for bash's reader, not just the X11 window, to become ready.
        thread::sleep(Duration::from_millis(100));
        let resume = if case == "delayed" {
            let pid = children.xterm.id().to_string();
            assert!(Command::new("kill")
                .args(["-STOP", &pid])
                .status()
                .unwrap()
                .success());
            Some(thread::spawn(move || {
                thread::sleep(Duration::from_millis(150));
                assert!(Command::new("kill")
                    .args(["-CONT", &pid])
                    .status()
                    .unwrap()
                    .success());
            }))
        } else {
            None
        };
        let started = Instant::now();
        let outcome = injector.inject_blocking(
            &text,
            if case == "direct" {
                InjectionPolicy::Type
            } else {
                InjectionPolicy::Paste
            },
        );
        assert!(
            matches!(outcome, InjectionOutcome::Injected { ref method, .. }
            if *method == if matches!(case, "direct" | "fallback") { InjectMethod::Keystroke } else { InjectMethod::Paste }),
            "{case}: {outcome:?}"
        );
        if let Some(resume) = resume {
            resume.join().unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while !output.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            fs::read_to_string(&output).expect("xterm receives complete dictation"),
            text,
            "{case}"
        );
        children.xterm.wait().unwrap();
        fs::remove_file(output).unwrap();
        match case {
            "empty" => {
                assert!(matches!(
                    clipboard.get_text(),
                    Err(arboard::Error::ContentNotAvailable)
                ));
                assert!(matches!(
                    clipboard.get_image(),
                    Err(arboard::Error::ContentNotAvailable)
                ));
            }
            "png" => {
                let after = clipboard.get_image().unwrap();
                let before = prior_image.unwrap();
                assert_eq!((after.width, after.height), (before.width, before.height));
                assert_eq!(after.bytes, before.bytes, "image pixels identical");
            }
            "html" => {
                assert_eq!(clipboard.get().html().unwrap(), "<b>Synthetic prior</b>");
                assert_eq!(clipboard.get_text().unwrap(), "Synthetic prior");
            }
            _ => assert_eq!(
                clipboard.get_text().unwrap(),
                "Synthetic prior λ\nclipboard"
            ),
        }
        eprintln!(
            "x11| {case}: {:?}; paste/readback/restore {:?}",
            outcome,
            started.elapsed()
        );
    }
    match prior_display {
        Some(value) => std::env::set_var("DISPLAY", value),
        None => std::env::remove_var("DISPLAY"),
    }
}
