use std::io::{BufRead, BufReader, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use dictate_gpu_gate::{
    gate::{acquire, GateConfig},
    status::{observe, StatusConfig},
};
use serde_json::{json, Value};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        // RSI TMPDIR can exceed sun_path's limit. Only synthetic data here.
        let path = PathBuf::from(format!("/tmp/dgg-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }
    fn script(&self, file: &str, text: &str) -> PathBuf {
        let path = self.path(file);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read(stream: &UnixStream) -> Value {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn gate_waits_for_grant_and_sends_v1_identity() {
    let f = Fixture::new();
    let path = f.path("gate.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read(&stream);
        assert_eq!(request["op"], "gpu_gate_acquire");
        assert_eq!(request["v"], 1);
        assert_eq!(request["holder"], "dictation");
        assert_eq!(request["deadline_ms"], 1000);
        assert_eq!(
            uuid::Uuid::parse_str(request["intent_id"].as_str().unwrap())
                .unwrap()
                .get_version_num(),
            4
        );
        std::thread::sleep(Duration::from_millis(90));
        writeln!(stream, "{{\"granted\":true}}").unwrap();
    });
    let start = Instant::now();
    assert_eq!(
        acquire(&GateConfig {
            socket: path,
            deadline_ms: 1000,
            ..Default::default()
        }),
        "granted"
    );
    assert!(start.elapsed() >= Duration::from_millis(80));
    server.join().unwrap();
}

#[test]
fn absent_and_refused_socket_proceed_immediately() {
    let f = Fixture::new();
    let path = f.path("missing.sock");
    for stale in [false, true] {
        if stale {
            drop(UnixListener::bind(&path).unwrap());
        }
        let start = Instant::now();
        assert_eq!(
            acquire(&GateConfig {
                socket: path.clone(),
                ..Default::default()
            }),
            "gate_absent"
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}

#[test]
fn silent_malformed_refused_oversized_and_trickled_replies_fail_open() {
    let f = Fixture::new();
    for (i, reply, expected) in [
        (0, "silent", "gate_timeout"),
        (1, "garbage\n", "gate_error"),
        (2, "{\"granted\":false}\n", "gate_refused"),
        (3, "{\"granted\":\"true\"}\n", "gate_error"),
        (4, "oversized", "gate_error"),
        (5, "trickle", "gate_timeout"),
        (6, "eof", "gate_error"),
    ] {
        let path = f.path(&format!("gate{i}.sock"));
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read(&stream);
            match reply {
                "silent" => std::thread::sleep(Duration::from_millis(150)),
                "oversized" => {
                    let _ = stream.write_all(&vec![b'x'; 65538]);
                }
                "trickle" => {
                    for _ in 0..8 {
                        if stream.write_all(b" ").is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                "eof" => (),
                _ => {
                    stream.write_all(reply.as_bytes()).unwrap();
                }
            }
        });
        let start = Instant::now();
        let outcome = acquire(&GateConfig {
            socket: path,
            deadline_ms: 100,
            ..Default::default()
        });
        assert!(outcome.starts_with(expected), "{reply}: {outcome}");
        assert!(start.elapsed() < Duration::from_secs(1));
        if expected == "gate_timeout" {
            assert!(start.elapsed() >= Duration::from_millis(80));
        }
        server.join().unwrap();
    }
}

#[test]
fn binary_always_exits_zero_and_config_env_overrides_work() {
    let f = Fixture::new();
    let config = f.path("gate.toml");
    std::fs::write(
        &config,
        "op = 'configured_op'\nsocket = '/absent'\ndeadline_ms = 100",
    )
    .unwrap();
    let path = f.path("override.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read(&stream);
        assert_eq!(request["op"], "overridden_op");
        assert_eq!(request["deadline_ms"], 200);
        writeln!(stream, "{{\"granted\":false}}").unwrap();
    });
    let result = Command::new(env!("CARGO_BIN_EXE_dictate-gpu-gate"))
        .arg("acquire")
        .env("DICTATE_GPU_GATE_CONFIG", &config)
        .env("DICTATE_GPU_GATE_SOCKET", &path)
        .env("DICTATE_GPU_GATE_OP", "overridden_op")
        .env("DICTATE_GPU_GATE_DEADLINE_MS", "200")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("gate_refused"));
    assert_eq!(result.stderr.iter().filter(|b| **b == b'\n').count(), 1);
    server.join().unwrap();
    for args in [vec![], vec!["acquire"], vec!["invalid"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_dictate-gpu-gate"))
            .args(args)
            .env("DICTATE_GPU_GATE_CONFIG", f.path("missing-config"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stderr.iter().filter(|b| **b == b'\n').count(), 1);
    }
}

fn status_config(f: &Fixture, state: &str) -> StatusConfig {
    let script = f.script("systemctl", &format!("#!/bin/sh\n[ \"$*\" = '--user show dictated.service --timestamp=unix -p ActiveState,ActiveEnterTimestamp,StateChangeTimestamp' ] || exit 4\nprintf '%s\\n' 'ActiveState={state}' 'ActiveEnterTimestamp=@1729728000' 'StateChangeTimestamp=@1729728001'\n"));
    std::fs::create_dir_all(f.path("proc")).unwrap();
    StatusConfig {
        systemctl: script,
        control_socket: f.path("control.sock"),
        legacy_pid: f.path("dictate.pid"),
        proc_root: f.path("proc"),
        timeout: Duration::from_millis(150),
    }
}

fn model_stub(path: &Path, loaded: bool, backend: &'static str) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read(&stream);
        assert_eq!(request["command"]["type"], "handshake");
        assert_eq!(request["command"]["protocol_version"], 1);
        writeln!(
            stream,
            "{}",
            json!({"kind":"response","v":1,"id":1,"result":{"type":"handshake"}})
        )
        .unwrap();
        let request = read(&stream);
        assert_eq!(request["command"]["type"], "get_status");
        writeln!(
            stream,
            "{}",
            json!({"kind":"event","v":1,"event":{"type":"ignored"}})
        )
        .unwrap();
        writeln!(stream, "{}", json!({"kind":"response","v":1,"id":2,"result":{"type":"status","model":{"loaded":loaded,"backend":backend}}})).unwrap();
    })
}

#[test]
fn status_sources_off_starting_ready_cpu_and_legacy() {
    let f = Fixture::new();
    let config = status_config(&f, "inactive");
    let off = observe(&config);
    assert_eq!(off["state"], "off");
    assert_eq!(off["since"], "2024-10-24T00:00:01Z");
    assert_eq!(off["gpu"], json!({"cuda":false,"model_loaded":false}));
    assert!(!config.control_socket.exists());
    assert_eq!(
        observe(&status_config(&f, "activating"))["state"],
        "starting"
    );
    let config = status_config(&f, "active");
    assert_eq!(
        observe(&config)["state"],
        "starting",
        "unreachable control is not ready"
    );
    for (loaded, backend, expected) in [
        (false, "cuda", "starting"),
        (true, "cuda", "on"),
        (true, "cpu", "on"),
    ] {
        let server = model_stub(&config.control_socket, loaded, backend);
        let observed = observe(&config);
        assert_eq!(observed["state"], expected);
        assert_eq!(
            observed["gpu"],
            json!({"cuda":backend == "cuda","model_loaded":loaded})
        );
        if loaded {
            assert_eq!(observed["since"], "2024-10-24T00:00:00Z");
        }
        server.join().unwrap();
        std::fs::remove_file(&config.control_socket).unwrap();
    }
    let config = status_config(&f, "inactive");
    let process = config.proc_root.join(std::process::id().to_string());
    std::fs::create_dir(&process).unwrap();
    std::fs::write(&config.legacy_pid, std::process::id().to_string()).unwrap();
    std::fs::write(process.join("comm"), "dictated\n").unwrap();
    assert_eq!(
        observe(&config)["state"],
        "off",
        "claimed legacy pid is not legacy"
    );
    std::fs::write(process.join("comm"), "dictate-agent\n").unwrap();
    assert_eq!(observe(&config)["state"], "on");
    std::fs::remove_file(&config.legacy_pid).unwrap();
    assert_eq!(
        observe(&config)["sources"]["legacy_dictate_agent"],
        "running",
        "comm scan works without pid file"
    );
}

#[test]
fn slow_systemctl_and_silent_control_are_bounded() {
    let f = Fixture::new();
    let mut config = status_config(&f, "active");
    let listener = UnixListener::bind(&config.control_socket).unwrap();
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(250));
    });
    let start = Instant::now();
    assert_eq!(observe(&config)["state"], "starting");
    assert!(start.elapsed() < Duration::from_secs(1));
    server.join().unwrap();
    // exec replaces the shell: timeout cleanup leaves no test process behind.
    config.systemctl = f.script("slow-systemctl", "#!/bin/sh\nexec sleep 5\n");
    let start = Instant::now();
    let result = observe(&config);
    assert_eq!(result["state"], "off");
    assert_eq!(result["sources"]["dictated_service"], "failed");
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn activated_status_socket_emits_one_line_with_optional_request() {
    let f = Fixture::new();
    status_config(&f, "inactive");
    for request in [false, true] {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        if request {
            writeln!(client, "{{\"op\":\"status\"}}").unwrap();
        }
        let input = OwnedFd::from(server.try_clone().unwrap());
        let output = OwnedFd::from(server);
        let mut child = Command::new(env!("CARGO_BIN_EXE_dictate-gpu-status"))
            .env("PATH", &f.0)
            .env("HOME", &f.0)
            .env("XDG_CONFIG_HOME", &f.0)
            .env("XDG_RUNTIME_DIR", &f.0)
            .env("DICTATE_SOCKET", f.path("absent.sock"))
            .env("DICTATE_GPU_STATUS_PROC_ROOT", f.path("proc"))
            .env("DICTATE_GPU_STATUS_LEGACY_PID", f.path("absent.pid"))
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .spawn()
            .unwrap();
        let result = read(&client);
        assert_eq!(result["v"], 1);
        assert_eq!(result["state"], "off");
        assert!(result["observed_at"].as_str().unwrap().ends_with('Z'));
        assert!(child.wait().unwrap().success());
    }
}

#[test]
fn legacy_launcher_requires_opt_in_and_gate_before_exec() {
    let f = Fixture::new();
    let bin = f.path(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    f.script(
        ".local/bin/dictate-gpu-gate",
        "#!/bin/sh\necho gate >>\"$HOME/trace\"\n",
    );
    f.script(
        ".local/bin/dictate-agent",
        "#!/bin/sh\necho legacy >>\"$HOME/trace\"\n",
    );
    f.script("systemctl", "#!/bin/sh\nexit 1\n");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/run.sh");
    let run = |opt_in: bool| {
        let mut cmd = Command::new("/bin/bash");
        cmd.arg(&script)
            .env("HOME", &f.0)
            .env("PATH", &f.0)
            .env_remove("DICTATE_ALLOW_LEGACY");
        if opt_in {
            cmd.env("DICTATE_ALLOW_LEGACY", "1");
        }
        cmd.output().unwrap()
    };
    assert!(!run(false).status.success());
    assert!(!f.path("trace").exists());
    assert!(run(true).status.success());
    assert_eq!(
        std::fs::read_to_string(f.path("trace")).unwrap(),
        "gate\nlegacy\n"
    );
    std::fs::remove_file(f.path("trace")).unwrap();
    f.script("systemctl", "#!/bin/sh\nexit 0\n");
    assert!(run(true).status.success());
    assert!(
        !f.path("trace").exists(),
        "enabled dictated blocks legacy and gate"
    );
    f.script("systemctl", "#!/bin/sh\nexit 1\n");
    std::fs::remove_file(bin.join("dictate-gpu-gate")).unwrap();
    assert!(!run(true).status.success());
    assert!(
        !f.path("trace").exists(),
        "a missing helper must not bypass the legacy gate"
    );
}
