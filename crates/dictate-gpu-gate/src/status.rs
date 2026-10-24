use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dictate_proto::{ClientInfo, ClientKind, Command as ProtocolCommand, Hello, Message};
use serde_json::{json, Value};

use crate::wire;

pub const STATUS_SOCKET: &str = "dictate-agent/gpu-status.sock";
const PROBE_TIMEOUT: Duration = Duration::from_millis(750);

pub struct StatusConfig {
    pub systemctl: PathBuf,
    pub control_socket: PathBuf,
    pub legacy_pid: PathBuf,
    pub proc_root: PathBuf,
    pub timeout: Duration,
}

impl Default for StatusConfig {
    fn default() -> Self {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            // SAFETY: geteuid has no preconditions.
            .map(|p| p.join("dictate-agent"))
            .unwrap_or_else(|| {
                PathBuf::from(format!("/tmp/dictate-agent-{}", unsafe { libc::geteuid() }))
            });
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/tmp".into());
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Self {
            systemctl: "systemctl".into(),
            control_socket: std::env::var_os("DICTATE_SOCKET")
                .map(PathBuf::from)
                .unwrap_or_else(|| runtime.join("dictated.sock")),
            legacy_pid: std::env::var_os("DICTATE_GPU_STATUS_LEGACY_PID")
                .map(PathBuf::from)
                .unwrap_or_else(|| config.join("dictate-agent/dictate.pid")),
            proc_root: std::env::var_os("DICTATE_GPU_STATUS_PROC_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/proc".into()),
            timeout: PROBE_TIMEOUT,
        }
    }
}

/// The complete protocol v1 truth table; legacy always wins because it owns VRAM.
pub fn derive_state(service: &str, legacy: bool, model_loaded: bool) -> &'static str {
    if legacy || service == "active" && model_loaded {
        "on"
    } else if service == "activating" || service == "active" {
        "starting"
    } else {
        "off"
    }
}

pub fn observe(config: &StatusConfig) -> Value {
    let properties = systemctl_properties(config).unwrap_or_default();
    let property = |key: &str| {
        properties
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
    };
    let service = match property("ActiveState") {
        Some(s @ ("inactive" | "activating" | "active" | "deactivating" | "failed")) => s,
        _ => {
            eprintln!("dictate-gpu-status source_error=systemctl");
            "failed"
        }
    };
    let legacy = legacy_running(config);
    let model = if service == "active" {
        model_status(&config.control_socket, config.timeout).ok()
    } else {
        None
    };
    if service == "active" && model.is_none() {
        eprintln!("dictate-gpu-status source_error=control_socket");
    }
    let loaded = model
        .as_ref()
        .and_then(|m| m["loaded"].as_bool())
        .unwrap_or(false);
    let cuda = model.as_ref().and_then(|m| m["backend"].as_str()) == Some("cuda");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let observed_at = timestamp(now);
    // systemd records service transitions, not model-ready transitions. With no
    // trustworthy transition timestamp (legacy, unknown source), use this observation.
    let since = if legacy {
        None
    } else {
        property(if service == "active" && loaded {
            "ActiveEnterTimestamp"
        } else {
            "StateChangeTimestamp"
        })
        .and_then(parse_systemd_timestamp)
        .map(timestamp)
    }
    .unwrap_or_else(|| observed_at.clone());
    json!({"v":1, "state":derive_state(service, legacy, loaded), "since":since,
        "observed_at":observed_at,
        "sources":{"dictated_service":service, "legacy_dictate_agent":if legacy { "running" } else { "absent" }},
        "gpu":{"cuda":cuda, "model_loaded":loaded}})
}

fn systemctl_properties(config: &StatusConfig) -> io::Result<String> {
    let deadline = Instant::now() + config.timeout;
    let mut child = Command::new(&config.systemctl)
        .args([
            "--user",
            "show",
            "dictated.service",
            "--timestamp=unix",
            "-p",
            "ActiveState,ActiveEnterTimestamp,StateChangeTimestamp",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    // Read only after exit; nonblocking also prevents an inherited pipe in a
    // broken systemctl wrapper from extending our absolute probe timeout.
    // SAFETY: stdout owns a live fd; fcntl has no pointer arguments here.
    let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        let error = io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return Err(io::Error::other("systemctl failed")),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(result
                    .err()
                    .unwrap_or_else(|| io::ErrorKind::TimedOut.into()));
            }
        }
    }
    let mut output = String::new();
    stdout
        .by_ref()
        .take(wire::MAX_LINE as u64)
        .read_to_string(&mut output)?;
    Ok(output)
}

fn legacy_running(config: &StatusConfig) -> bool {
    let is_legacy = |pid: &str| {
        std::fs::read_to_string(config.proc_root.join(pid).join("comm"))
            .is_ok_and(|comm| comm.trim() == "dictate-agent")
    };
    if let Some(pid) = std::fs::read_to_string(&config.legacy_pid)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .filter(|p| *p > 0)
    {
        // SAFETY: positive pid and signal zero probe liveness, never deliver a signal.
        let alive = unsafe { libc::kill(pid, 0) } == 0
            || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        // dictated also claims this legacy file. Identity is necessary to avoid
        // mistaking dictated (or a reused PID) for the legacy GPU owner.
        if alive && is_legacy(&pid.to_string()) {
            return true;
        }
    }
    std::fs::read_dir(&config.proc_root).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            let pid = entry.file_name();
            let pid = pid.to_string_lossy();
            pid.parse::<u32>().is_ok_and(|p| p > 0) && is_legacy(&pid)
        })
    })
}

fn model_status(path: &Path, timeout: Duration) -> io::Result<Value> {
    let deadline = Instant::now() + timeout;
    let mut stream = wire::connect(path, deadline)?;
    let hello = Hello::new(ClientInfo::new("dictate-gpu-status", ClientKind::Cli));
    let handshake =
        serde_json::to_value(Message::request(1u64, ProtocolCommand::Handshake(hello)))?;
    wire::send(&mut stream, &handshake, deadline)?;
    let reply = response(&mut stream, 1, deadline)?;
    if reply["result"]["type"] != "handshake" {
        return Err(io::Error::other("handshake refused"));
    }
    wire::send(
        &mut stream,
        &serde_json::to_value(Message::request(2u64, ProtocolCommand::GetStatus))?,
        deadline,
    )?;
    let reply = response(&mut stream, 2, deadline)?;
    if reply["result"]["type"] != "status" {
        return Err(io::Error::other("status refused"));
    }
    reply["result"]["model"]
        .as_object()
        .map(|m| Value::Object(m.clone()))
        .ok_or_else(|| io::Error::other("missing model"))
}

fn response(
    stream: &mut std::os::unix::net::UnixStream,
    id: u64,
    deadline: Instant,
) -> io::Result<Value> {
    loop {
        let value = wire::receive(stream, deadline)?;
        if value["kind"] == "response" && value["v"] == 1 && value["id"] == id {
            return Ok(value);
        }
    }
}

fn parse_systemd_timestamp(raw: &str) -> Option<u64> {
    raw.strip_prefix('@')?.split('.').next()?.parse().ok()
}

fn timestamp(seconds: u64) -> String {
    let seconds = seconds as libc::time_t;
    // SAFETY: zero is a valid initial tm; gmtime_r writes only the supplied tm.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::gmtime_r(&seconds, &mut tm) }.is_null() {
        return "1970-01-01T00:00:00Z".into();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_source_combinations() {
        for (service, unloaded, loaded) in [
            ("inactive", "off", "off"),
            ("activating", "starting", "starting"),
            ("active", "starting", "on"),
            ("deactivating", "off", "off"),
            ("failed", "off", "off"),
        ] {
            assert_eq!(derive_state(service, false, false), unloaded);
            assert_eq!(derive_state(service, false, true), loaded);
            for ready in [false, true] {
                assert_eq!(derive_state(service, true, ready), "on");
            }
        }
    }
    #[test]
    fn systemd_time_is_rfc3339() {
        assert_eq!(
            timestamp(parse_systemd_timestamp("@1729728000.123").unwrap()),
            "2024-10-24T00:00:00Z"
        );
        assert_eq!(parse_systemd_timestamp("n/a"), None);
    }
}
