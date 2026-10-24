use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::wire;

pub const ACQUIRE_OP: &str = "gpu_gate_acquire";
pub const PLAYBOY_SOCKET: &str = "playboy/gpu-gate.sock";
pub const DEADLINE_MS: u64 = 90_000;

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GateConfig {
    pub socket: PathBuf,
    pub op: String,
    pub deadline_ms: u64,
}

impl Default for GateConfig {
    fn default() -> Self {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            // SAFETY: geteuid has no preconditions.
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })));
        Self {
            socket: runtime.join(PLAYBOY_SOCKET),
            op: ACQUIRE_OP.into(),
            deadline_ms: DEADLINE_MS,
        }
    }
}

impl GateConfig {
    /// Optional dedicated TOML file; env wins. Never edits dictation's config.
    pub fn from_env() -> Result<Self, String> {
        let mut config: Self = if let Some(path) = std::env::var_os("DICTATE_GPU_GATE_CONFIG") {
            toml::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
        } else {
            Self::default()
        };
        if let Some(path) = std::env::var_os("DICTATE_GPU_GATE_SOCKET") {
            config.socket = path.into();
        }
        if let Ok(op) = std::env::var("DICTATE_GPU_GATE_OP") {
            config.op = op;
        }
        if let Ok(ms) = std::env::var("DICTATE_GPU_GATE_DEADLINE_MS") {
            config.deadline_ms = ms.parse().map_err(|_| "invalid deadline_ms")?;
        }
        // Keep TimeoutStartSec meaningful even with a bad deployment override.
        if config.op.is_empty() || config.deadline_ms == 0 || config.deadline_ms > DEADLINE_MS {
            return Err("op must be nonempty; deadline_ms must be 1..=90000".into());
        }
        Ok(config)
    }
}

/// One bounded journal line; all outcomes deliberately permit dictation to start.
pub fn acquire(config: &GateConfig) -> &'static str {
    match try_acquire(config) {
        Ok(true) => "granted",
        Ok(false) => "gate_refused warning=fail_open",
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ECONNREFUSED)) => {
            "gate_absent"
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            "gate_timeout warning=fail_open"
        }
        Err(_) => "gate_error warning=fail_open",
    }
}

fn try_acquire(config: &GateConfig) -> io::Result<bool> {
    let deadline = Instant::now() + Duration::from_millis(config.deadline_ms.min(DEADLINE_MS));
    let mut stream = wire::connect(&config.socket, deadline)?;
    wire::send(
        &mut stream,
        &json!({"op": config.op, "v": 1, "holder": "dictation",
        "intent_id": uuid::Uuid::new_v4().to_string(), "deadline_ms": config.deadline_ms}),
        deadline,
    )?;
    let reply = wire::receive(&mut stream, deadline)?;
    reply
        .get("granted")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing boolean granted"))
}
