//! Real X11 delivery smoke. It starts its own X server so CI does not need a
//! desktop session; when Xvfb/xterm/xdotool are absent it intentionally skips.

use arboard::Clipboard;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use dictate_inject::{InjectionPolicy, OutputConfig, X11Injector};
use dictate_proto::{InjectMethod, InjectionOutcome};

struct ChildCleanup {
    xterm: std::process::Child,
    xvfb: Option<std::process::Child>,
}

impl Drop for ChildCleanup {
    fn drop(&mut self) {
        let _ = self.xterm.kill();
        let _ = self.xterm.wait();
        if let Some(xvfb) = &mut self.xvfb {
            let _ = xvfb.kill();
            let _ = xvfb.wait();
        }
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
    if let Ok(display) = std::env::var("DICTATE_PRIVATE_X11_SMOKE") {
        run_smoke(&display);
        return;
    }
    let mut xvfb = Command::new("Xvfb")
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
        .expect("start private Xvfb");
    let mut number = String::new();
    BufReader::new(xvfb.stdout.take().unwrap())
        .read_line(&mut number)
        .unwrap();
    let _cleanup = ChildCleanup {
        xterm: Command::new("true").spawn().unwrap(),
        xvfb: Some(xvfb),
    };
    assert!(!number.trim().is_empty(), "Xvfb allocated private display");
    let display = format!(":{}", number.trim());
    // The child owns its environment. A panic cannot leak DISPLAY changes to
    // parallel tests, and Wayland/auth inherited from the desktop is removed.
    assert!(Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "xvfb_xterm_pastes_and_restores_empty_text_html_and_png_clipboards",
            "--nocapture"
        ])
        .env("DISPLAY", &display)
        .env("DICTATE_PRIVATE_X11_SMOKE", &display)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("XAUTHORITY")
        .status()
        .unwrap()
        .success());
}

fn run_smoke(display: &str) {
    let stamp = format!("dictate-inject-smoke-{}", std::process::id());
    let mut clipboard = Clipboard::new().unwrap();
    let injector = X11Injector::new(&OutputConfig::default());
    let mut children = ChildCleanup {
        xterm: Command::new("true").spawn().unwrap(),
        xvfb: None,
    };
    for case in [
        "empty",
        "text",
        "png",
        "html",
        "long",
        "delayed",
        "direct",
        "timeout",
        "late",
        "focus-paste",
        "focus-type",
    ] {
        match case {
            "empty" => clipboard.clear().unwrap(),
            "png" => {
                let mut owner = Command::new("xclip")
                    .env("DISPLAY", display)
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
        let prior_image = if case == "png" {
            // xclip forks its selection owner: launcher exit is not readiness.
            let deadline = Instant::now() + Duration::from_secs(2);
            Some(loop {
                match clipboard.get_image() {
                    Ok(image) => break image,
                    Err(arboard::Error::ContentNotAvailable) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("PNG fixture owner did not become ready: {error}"),
                }
            })
        } else {
            None
        };
        let text = if case == "long" {
            "synthetic ".repeat(1500).trim().to_owned()
        } else {
            "wispr smoke".to_owned()
        };
        let output = std::env::temp_dir().join(format!("{stamp}-{case}"));
        let _ = fs::remove_file(&output);
        let script = if case == "late" {
            "stty -echo; IFS= read -r -n \"$2\" line; extra=''; IFS= read -r -t 0.3 -n 1 extra || true; printf '%s%s' \"$line\" \"$extra\" > \"$1\""
        } else {
            "stty -echo; IFS= read -r -n \"$2\" line; printf '%s' \"$line\" > \"$1\""
        };
        children.xterm = Command::new("xterm")
            .env("DISPLAY", display)
            .args([
                "-title",
                &stamp,
                "-xrm",
                if case == "timeout" {
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
                .env("DISPLAY", display)
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
            .env("DISPLAY", display)
            .args(["windowfocus", "--sync", &window.expect("xterm window")])
            .status()
            .unwrap()
            .success());
        // Wait for bash's reader, not just the X11 window, to become ready.
        thread::sleep(Duration::from_millis(100));
        let resume = if matches!(case, "delayed" | "late") {
            let pid = children.xterm.id().to_string();
            assert!(Command::new("kill")
                .args(["-STOP", &pid])
                .status()
                .unwrap()
                .success());
            Some(thread::spawn(move || {
                thread::sleep(Duration::from_millis(if case == "late" {
                    2300
                } else {
                    150
                }));
                assert!(Command::new("kill")
                    .args(["-CONT", &pid])
                    .status()
                    .unwrap()
                    .success());
            }))
        } else {
            None
        };
        let stop_window = dictate_inject::focused_window();
        let changed_focus = if matches!(case, "focus-paste" | "focus-type") {
            use x11rb::connection::Connection;
            use x11rb::protocol::xproto::{
                ConnectionExt, CreateWindowAux, InputFocus, WindowClass,
            };
            let (conn, screen) = x11rb::connect(Some(display)).unwrap();
            let window = conn.generate_id().unwrap();
            conn.create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                conn.setup().roots[screen].root,
                0,
                0,
                30,
                30,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new(),
            )
            .unwrap()
            .check()
            .unwrap();
            conn.map_window(window).unwrap().check().unwrap();
            conn.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
                .unwrap()
                .check()
                .unwrap();
            Some(conn)
        } else {
            None
        };
        let started = Instant::now();
        let outcome = injector.inject_bound_blocking(
            &text,
            if matches!(case, "direct" | "focus-type") {
                InjectionPolicy::Type
            } else {
                InjectionPolicy::Paste
            },
            Some(stop_window),
        );
        if matches!(case, "timeout" | "focus-paste" | "focus-type") {
            assert!(
                matches!(outcome, InjectionOutcome::Failed { .. }),
                "{case}: {outcome:?}"
            );
            if case.starts_with("focus") {
                assert!(matches!(outcome, InjectionOutcome::Failed { ref error }
                    if error.message == "Dictation copied: focus changed"));
            }
            assert_eq!(clipboard.get_text().unwrap(), text, "dictation retained");
            assert!(!output.exists(), "no keys or typing retry sent to xterm");
            children.xterm.kill().unwrap();
            children.xterm.wait().unwrap();
            drop(changed_focus);
            eprintln!("x11| {case}: {outcome:?}");
            continue;
        }
        if case == "late" {
            assert!(
                matches!(outcome, InjectionOutcome::Failed { .. }),
                "unconfirmed paste must not retry"
            );
        } else {
            assert!(
                matches!(outcome, InjectionOutcome::Injected { ref method, .. }
                if *method == if case == "direct" { InjectMethod::Keystroke } else { InjectMethod::Paste }),
                "{case}: {outcome:?}"
            );
        }
        if let Some(resume) = resume {
            resume.join().unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while fs::metadata(&output).map_or(true, |metadata| metadata.len() < text.len() as u64)
            && Instant::now() < deadline
        {
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
                let png = Command::new("xclip")
                    .env("DISPLAY", display)
                    .args(["-selection", "clipboard", "-t", "image/png", "-o"])
                    .output()
                    .unwrap();
                assert!(png.status.success());
                assert_eq!(
                    png.stdout,
                    include_bytes!("fixtures/clipboard.png"),
                    "PNG bytes and metadata preserved without re-encoding"
                );
            }
            "html" => {
                assert_eq!(clipboard.get().html().unwrap(), "<b>Synthetic prior</b>");
                assert_eq!(clipboard.get_text().unwrap(), "Synthetic prior");
            }
            "late" => assert_eq!(clipboard.get_text().unwrap(), text),
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
}
