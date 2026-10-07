//! `dictate doctor`'s brain: one named check per dependency, each with a
//! verdict and a one-line fix.
//!
//! The daemon runs the checks (rather than the CLI) because the daemon is the
//! process whose environment matters: *its* `$DISPLAY`, *its* loaded model,
//! *its* view of the GPU. The CLI adds the one check only it can make — that the
//! daemon is reachable at all — and renders the result.
//!
//! Every check is read-only. Nothing here deletes a stale PID file, pulls a
//! model, or starts a service; the fix is a sentence for the user, never an
//! action the doctor takes on its own.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dictate_core::config::{Config, ConfigReport};
use dictate_core::ollama;
use dictate_core::ports::BoxFuture;
use dictate_core::Pipeline;
use dictate_proto::{DiagnosticCheck, DiagnosticsReport, FormatterHealth};

use crate::paths::{self, RuntimePaths};
use crate::server::DiagnosticsProvider;

/// How long to wait for a model that is still loading before calling it broken.
const MODEL_LOAD_PATIENCE: Duration = Duration::from_secs(20);
/// Hard bound on any single probe of an external service.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// External programs the daemon shells out to, and what each is for.
const TOOLS: &[(&str, &str)] = &[
    ("playerctl", "pausing media while recording"),
    ("systemd-run", "the timer route"),
    ("dunstify", "timer notifications"),
    ("play", "the timer alarm sound (sox)"),
    ("ollama", "the formatting pass and the local route"),
];

/// Runs the checks for one running daemon.
pub struct Doctor {
    config: Config,
    report: ConfigReport,
    runtime: RuntimePaths,
    pipeline: Arc<Pipeline>,
}

impl Doctor {
    /// A doctor for a daemon built from `config`.
    #[must_use]
    pub fn new(
        config: Config,
        report: ConfigReport,
        runtime: RuntimePaths,
        pipeline: Arc<Pipeline>,
    ) -> Self {
        Self {
            config,
            report,
            runtime,
            pipeline,
        }
    }

    /// Run every check, in the order a person should read them.
    pub async fn run(&self, quick: bool) -> DiagnosticsReport {
        // The slow, independent ones run concurrently: hashing a 1.6 GB model
        // and waiting on Ollama should overlap rather than add.
        let (stt_model, backend, ollama_checks, tools) = tokio::join!(
            self.check_stt_model(quick),
            self.check_stt_backend(),
            self.check_ollama(),
            check_tools(),
        );

        let mut checks = vec![
            self.check_config(),
            stt_model,
            backend,
            self.check_formatter(),
        ];
        checks.extend(ollama_checks);
        checks.push(self.check_injection().await);
        checks.push(self.check_hotkeys());
        checks.push(self.check_audio_input());
        checks.push(self.check_notifications());
        checks.push(tools);
        checks.push(self.check_legacy_pid());
        DiagnosticsReport { checks }
    }

    fn check_config(&self) -> DiagnosticCheck {
        const ID: &str = "config";
        const TITLE: &str = "Configuration";
        let r = &self.report;
        if !r.errors.is_empty() {
            return DiagnosticCheck::fail(
                ID,
                TITLE,
                r.errors.join("; "),
                "fix the listed values in the config file, then restart dictated",
            );
        }
        let origin = if r.path.as_os_str().is_empty() {
            "built-in configuration (no config file read)".to_string()
        } else if r.existed {
            format!("loaded {}", r.path.display())
        } else {
            format!("no file at {}; using defaults", r.path.display())
        };
        if r.warnings.is_empty() {
            return DiagnosticCheck::ok(ID, TITLE, origin);
        }
        let shown: Vec<&str> = r.warnings.iter().take(3).map(String::as_str).collect();
        let more = r.warnings.len().saturating_sub(shown.len());
        let mut detail = format!(
            "{origin}; {} warning(s): {}",
            r.warnings.len(),
            shown.join(" | ")
        );
        if more > 0 {
            detail.push_str(&format!(" | (+{more} more)"));
        }
        DiagnosticCheck::warn(
            ID,
            TITLE,
            detail,
            "run `dictated --check-config` for the full list; rename or remove the flagged keys",
        )
    }

    async fn check_stt_model(&self, quick: bool) -> DiagnosticCheck {
        const ID: &str = "stt_model";
        const TITLE: &str = "Speech model";
        let whisper = &self.config.whisper;
        let path = Path::new(&whisper.model_path).to_path_buf();
        let Ok(meta) = std::fs::metadata(&path) else {
            let fix = if dictate_stt::catalog_model(&whisper.model).is_some() {
                format!("run `dictate model pull {}`", whisper.model)
            } else {
                format!(
                    "place a whisper.cpp model at {} or set whisper.model_path",
                    path.display()
                )
            };
            return DiagnosticCheck::fail(
                ID,
                TITLE,
                format!("model file not found at {}", path.display()),
                fix,
            );
        };

        let spec = dictate_stt::catalog_model(&whisper.model)
            .filter(|spec| path.file_name().is_some_and(|n| n == spec.filename));
        let Some(spec) = spec else {
            return DiagnosticCheck::ok(
                ID,
                TITLE,
                format!(
                    "custom model at {} ({} MiB); not a catalog entry, so not verified",
                    path.display(),
                    meta.len() / (1024 * 1024)
                ),
            );
        };

        if meta.len() != spec.bytes {
            return DiagnosticCheck::fail(
                ID,
                TITLE,
                format!(
                    "{} is {} bytes; the pinned {} is {} bytes (truncated or a different revision)",
                    path.display(),
                    meta.len(),
                    spec.id,
                    spec.bytes
                ),
                format!("run `dictate model pull {}` to replace it", spec.id),
            );
        }
        if quick {
            return DiagnosticCheck::ok(
                ID,
                TITLE,
                format!(
                    "{} present, size matches the catalog; SHA-256 not checked (quick run)",
                    spec.id
                ),
            );
        }
        let started = std::time::Instant::now();
        let expected = spec.sha256;
        let hashed = tokio::task::spawn_blocking({
            let path = path.clone();
            move || dictate_stt::model::verify_file(&path, expected)
        })
        .await;
        match hashed {
            Ok(Ok(true)) => DiagnosticCheck::ok(
                ID,
                TITLE,
                format!(
                    "{} verified against the pinned SHA-256 ({:.1}s)",
                    spec.id,
                    started.elapsed().as_secs_f64()
                ),
            ),
            Ok(Ok(false)) => DiagnosticCheck::fail(
                ID,
                TITLE,
                format!(
                    "{} does not match the pinned SHA-256 (corrupt)",
                    path.display()
                ),
                format!("run `dictate model pull {}` to replace it", spec.id),
            ),
            Ok(Err(e)) => DiagnosticCheck::fail(
                ID,
                TITLE,
                format!("could not read {}: {e}", path.display()),
                "check the file's permissions",
            ),
            Err(e) => DiagnosticCheck::fail(
                ID,
                TITLE,
                format!("verification task failed: {e}"),
                "re-run `dictate doctor`",
            ),
        }
    }

    async fn check_stt_backend(&self) -> DiagnosticCheck {
        const ID: &str = "stt_backend";
        const TITLE: &str = "Speech backend";
        let wanted = self.config.whisper.device.to_ascii_lowercase();

        // The model loads on a background thread at startup; give it a chance
        // rather than reporting a race as a fault.
        let deadline = tokio::time::Instant::now() + MODEL_LOAD_PATIENCE;
        let mut info = self.pipeline.stt.model();
        while !info.loaded && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
            info = self.pipeline.stt.model();
        }
        if !info.loaded {
            return DiagnosticCheck::fail(
                ID,
                TITLE,
                format!(
                    "model '{}' has not loaded after {}s",
                    info.name,
                    MODEL_LOAD_PATIENCE.as_secs()
                ),
                "check the daemon log (`journalctl --user -u dictated`) for the load error",
            );
        }
        let backend = info.backend.unwrap_or_else(|| "unknown".into());
        if backend != wanted {
            return DiagnosticCheck::fail(
                ID,
                TITLE,
                format!("configured for {wanted} but running on {backend}"),
                "set whisper.device to match, or fix the build (CUDA needs /opt/cuda/bin on PATH)",
            );
        }
        if backend != "cuda" {
            return DiagnosticCheck::ok(
                ID,
                TITLE,
                format!("{backend}; model '{}' loaded", info.name),
            );
        }

        // ModelInfo observes whisper.cpp initialization. Independently verify
        // residency through the driver as a second diagnostic signal.
        match gpu_memory_of(std::process::id()).await {
            GpuProbe::Held(mib) => DiagnosticCheck::ok(
                ID,
                TITLE,
                format!(
                    "cuda; model '{}' loaded; this daemon holds {mib} MiB of GPU memory (nvidia-smi)",
                    info.name
                ),
            ),
            GpuProbe::NotHeld => DiagnosticCheck::fail(
                ID,
                TITLE,
                "CUDA was requested but this process holds no GPU memory — whisper.cpp fell back to CPU",
                "rebuild with CUDA (PATH must include /opt/cuda/bin) and check `nvidia-smi` sees the GPU",
            ),
            GpuProbe::Unavailable(why) => DiagnosticCheck::warn(
                ID,
                TITLE,
                format!("reports cuda, but that could not be verified: {why}"),
                "install/enable nvidia-smi to confirm the GPU is in use",
            ),
        }
    }

    fn check_formatter(&self) -> DiagnosticCheck {
        const ID: &str = "formatter";
        const TITLE: &str = "Formatting pass";
        let Some(status) = self.pipeline.formatter.status() else {
            return DiagnosticCheck::skipped(ID, TITLE, "this formatter does not report health");
        };
        let model = status.model.clone().unwrap_or_default();
        let detail = status.detail.clone().unwrap_or_default();
        match status.health {
            FormatterHealth::Disabled => DiagnosticCheck::skipped(
                ID,
                TITLE,
                "disabled in config ([format.llm] enabled = false)",
            ),
            FormatterHealth::Ok => {
                DiagnosticCheck::ok(ID, TITLE, format!("'{model}' is answering"))
            }
            FormatterHealth::Unchecked => DiagnosticCheck::warn(
                ID,
                TITLE,
                format!("'{model}' has not been probed or used yet"),
                "see the grammar_model check below, or dictate once and re-run",
            ),
            FormatterHealth::ModelMissing => DiagnosticCheck::fail(
                ID,
                TITLE,
                format!("{detail} — every dictation is typed unformatted"),
                format!("`ollama pull {model}`, or put an installed model in format.llm.models"),
            ),
            FormatterHealth::Unreachable => DiagnosticCheck::fail(
                ID,
                TITLE,
                format!("{detail} — every dictation is typed unformatted"),
                "start Ollama (`ollama serve`) or set format.llm.enabled = false",
            ),
            FormatterHealth::Failing | FormatterHealth::Unknown(_) => DiagnosticCheck::fail(
                ID,
                TITLE,
                format!("the last run failed: {detail}"),
                "check the daemon log; set format.llm.enabled = false to stop the failures",
            ),
        }
    }

    /// Probe Ollama once (per distinct host) and derive the three checks that
    /// depend on it.
    async fn check_ollama(&self) -> Vec<DiagnosticCheck> {
        let llm = &self.config.format.llm;
        let local = &self.config.local;
        let (llm_probe, local_probe) = if llm.host == local.host {
            let p = ollama::probe(&llm.host, PROBE_TIMEOUT).await;
            (p.clone(), p)
        } else {
            tokio::join!(
                ollama::probe(&llm.host, PROBE_TIMEOUT),
                ollama::probe(&local.host, PROBE_TIMEOUT)
            )
        };

        let mut checks = Vec::new();
        checks.push(if llm_probe.reachable {
            DiagnosticCheck::ok(
                "ollama",
                "Ollama server",
                format!(
                    "reachable at {}; {} model(s) installed",
                    llm.host,
                    llm_probe.models.len()
                ),
            )
        } else {
            let why = llm_probe.error.clone().unwrap_or_default();
            let detail = format!("no answer from {}: {why}", llm.host);
            let fix = "start it with `ollama serve` (or `systemctl --user start ollama`)";
            if llm.enabled {
                DiagnosticCheck::fail("ollama", "Ollama server", detail, fix)
            } else {
                DiagnosticCheck::warn("ollama", "Ollama server", detail, fix)
            }
        });

        checks.push(check_formatter_ladder(llm, &llm_probe));

        checks.push(if !local_probe.reachable {
            DiagnosticCheck::skipped(
                "local_model",
                "Local-route model",
                "Ollama is unreachable, so installed models are unknown",
            )
        } else if local_probe.has_model(&local.model) {
            DiagnosticCheck::ok(
                "local_model",
                "Local-route model",
                format!("'{}' is installed", local.model),
            )
        } else {
            let installed = ollama::describe_installed(&local_probe.models);
            DiagnosticCheck::warn(
                "local_model",
                "Local-route model",
                format!(
                    "model '{}' is not installed (installed: {installed}); the `local` route will fail",
                    local.model
                ),
                format!(
                    "`ollama pull {}`, or set local.model to one of: {installed}",
                    local.model
                ),
            )
        });
        checks
    }

    async fn check_injection(&self) -> DiagnosticCheck {
        const ID: &str = "injection";
        const TITLE: &str = "Text injection";
        if !self.config.output.auto_type {
            return DiagnosticCheck::warn(
                ID,
                TITLE,
                "[output] auto_type = false: dictations are never typed",
                "set output.auto_type = true to type dictations",
            );
        }
        let display = std::env::var("DISPLAY").ok();
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let Some(display) = display.filter(|d| !d.is_empty()) else {
            return if wayland {
                DiagnosticCheck::warn(
                    ID,
                    TITLE,
                    "Wayland session: only X11 injection is implemented",
                    "run under XWayland ($DISPLAY) or use the upload/return-text path",
                )
            } else {
                DiagnosticCheck::warn(
                    ID,
                    TITLE,
                    "no $DISPLAY: this daemon is headless, so uploads work but text cannot be typed",
                    "start dictated inside your graphical session (`systemctl --user import-environment DISPLAY`)",
                )
            };
        };
        if let Some(problem) = x11_unreachable(&display) {
            return DiagnosticCheck::fail(
                ID,
                TITLE,
                format!("$DISPLAY={display} but the X server is not reachable: {problem}"),
                "restart dictated from a session that can reach the X server",
            );
        }
        let injector = self.pipeline.injector.clone();
        let available = tokio::task::spawn_blocking(move || injector.is_available())
            .await
            .unwrap_or(false);
        if available {
            DiagnosticCheck::ok(
                ID,
                TITLE,
                format!(
                    "X11 on {display}: clipboard save/restore + Ctrl+V paste, direct typing fallback"
                ),
            )
        } else {
            DiagnosticCheck::fail(
                ID,
                TITLE,
                format!("$DISPLAY={display} is reachable but no injection backend is available"),
                "check that the X server has the XTEST extension and a clipboard owner is allowed",
            )
        }
    }

    fn check_hotkeys(&self) -> DiagnosticCheck {
        const ID: &str = "hotkeys";
        const TITLE: &str = "Global hotkeys";
        let h = &self.config.hotkey;
        if !h.enabled {
            return DiagnosticCheck::skipped(
                ID,
                TITLE,
                "disabled ([hotkey] enabled = false); WM keybindings + signals are the control path",
            );
        }
        if h.devices.is_empty() {
            return DiagnosticCheck::warn(
                ID,
                TITLE,
                "enabled but no devices are listed, so no key can be heard",
                "list keyboard nodes under hotkey.devices (see /dev/input/by-id)",
            );
        }
        let unreadable: Vec<String> = h
            .devices
            .iter()
            .filter_map(|d| {
                std::fs::File::open(d)
                    .err()
                    .map(|e| format!("{d}: {}", short_io_error(&e)))
            })
            .collect();
        if unreadable.is_empty() {
            DiagnosticCheck::ok(
                ID,
                TITLE,
                format!(
                    "all {} configured input device(s) are readable",
                    h.devices.len()
                ),
            )
        } else {
            DiagnosticCheck::fail(
                ID,
                TITLE,
                format!("cannot read: {}", unreadable.join("; ")),
                "add your user to the `input` group (`sudo usermod -aG input $USER`) and log in again",
            )
        }
    }

    fn check_audio_input(&self) -> DiagnosticCheck {
        const ID: &str = "audio_input";
        const TITLE: &str = "Microphone";
        let Some(status) = self.pipeline.audio.input_status() else {
            return DiagnosticCheck::skipped(ID, TITLE, "this audio source does not report state");
        };
        if !status.capture_enabled {
            return DiagnosticCheck::ok(
                ID,
                TITLE,
                "audio-less mode ([audio] capture = false): no input device is ever opened; uploads only",
            );
        }
        let pre_roll = status.pre_roll_ms.unwrap_or(0);
        match (status.input_open, pre_roll) {
            (true, ms) if ms > 0 => DiagnosticCheck::ok(
                ID,
                TITLE,
                format!(
                    "input device is held open while idle for the {ms} ms pre-roll (your desktop's microphone indicator stays lit; \
                     set [audio] pre_roll_ms = 0 to close it between recordings)"
                ),
            ),
            (false, 0) => DiagnosticCheck::ok(
                ID,
                TITLE,
                "pre-roll disabled: the input device is closed while idle and opened per recording",
            ),
            (false, ms) => DiagnosticCheck::warn(
                ID,
                TITLE,
                format!("the {ms} ms pre-roll wants an open input device, but none is open"),
                "connect a microphone or set audio.input_device; it retries on the next recording",
            ),
            (true, _) => DiagnosticCheck::warn(
                ID,
                TITLE,
                "an input device is open although pre-roll is disabled (or a recording is in progress)",
                "if this persists while idle, file a bug: pre_roll_ms = 0 should close the device",
            ),
        }
    }

    fn check_notifications(&self) -> DiagnosticCheck {
        const ID: &str = "notifications";
        const TITLE: &str = "Desktop notifications";
        if !self.config.notifications.enabled {
            return DiagnosticCheck::skipped(
                ID,
                TITLE,
                "disabled ([notifications] enabled = false)",
            );
        }
        let has_bus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
            || std::env::var_os("XDG_RUNTIME_DIR")
                .is_some_and(|d| Path::new(&d).join("bus").exists());
        if has_bus {
            DiagnosticCheck::ok(ID, TITLE, "a session bus is reachable")
        } else {
            DiagnosticCheck::warn(
                ID,
                TITLE,
                "no session bus: notifications (including the formatter-failure alert) cannot be shown",
                "start dictated from within your desktop session",
            )
        }
    }

    fn check_legacy_pid(&self) -> DiagnosticCheck {
        const ID: &str = "legacy_pid";
        const TITLE: &str = "Legacy toggle PID file";
        let path = &self.runtime.legacy_pid;
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(_) => {
                return DiagnosticCheck::ok(
                    ID,
                    TITLE,
                    format!(
                        "{} does not exist: scripts/dictate-toggle has nothing to signal until a daemon claims it",
                        path.display()
                    ),
                )
            }
        };
        let Ok(pid) = raw.trim().parse::<u32>() else {
            return DiagnosticCheck::warn(
                ID,
                TITLE,
                format!("{} does not contain a process id", path.display()),
                format!("remove it: rm {}", path.display()),
            );
        };
        if pid == std::process::id() {
            return DiagnosticCheck::ok(
                ID,
                TITLE,
                format!("held by this daemon (pid {pid}); scripts/dictate-toggle drives it"),
            );
        }
        if !paths::process_is_alive(pid) {
            return DiagnosticCheck::warn(
                ID,
                TITLE,
                format!(
                    "{} names pid {pid}, which is not running (stale)",
                    path.display()
                ),
                format!(
                    "remove it (rm {}); dictated claims it on its next start",
                    path.display()
                ),
            );
        }
        let holder = process_name(pid).unwrap_or_else(|| "another process".into());
        DiagnosticCheck::warn(
            ID,
            TITLE,
            format!(
                "held by {holder} (pid {pid}): scripts/dictate-toggle and dictate-cancel signal THAT process, not dictated"
            ),
            "expected while the old daemon is still your daily driver; stop it before cutting over",
        )
    }
}

impl DiagnosticsProvider for Doctor {
    fn diagnose(&self, quick: bool) -> BoxFuture<'_, DiagnosticsReport> {
        Box::pin(self.run(quick))
    }
}

async fn check_tools() -> DiagnosticCheck {
    const ID: &str = "tools";
    const TITLE: &str = "External programs";
    let missing = tokio::task::spawn_blocking(|| {
        TOOLS
            .iter()
            .filter(|(program, _)| !on_path(program))
            .map(|(program, purpose)| format!("{program} ({purpose})"))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    if missing.is_empty() {
        DiagnosticCheck::ok(
            ID,
            TITLE,
            "playerctl, systemd-run, dunstify, play, ollama are on PATH",
        )
    } else {
        DiagnosticCheck::warn(
            ID,
            TITLE,
            format!("not on PATH: {}", missing.join(", ")),
            "install them with your package manager (Arch: `sudo pacman -S playerctl sox dunst`)",
        )
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file()
        })
    })
}

/// What `nvidia-smi` says about a process's GPU memory.
#[derive(Debug, PartialEq, Eq)]
enum GpuProbe {
    /// The process holds this many MiB.
    Held(u64),
    /// `nvidia-smi` ran and the process is not among the compute apps.
    NotHeld,
    /// The question could not be asked.
    Unavailable(String),
}

async fn gpu_memory_of(pid: u32) -> GpuProbe {
    let run = tokio::process::Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Err(_) => GpuProbe::Unavailable("nvidia-smi did not answer in time".into()),
        Ok(Err(e)) => GpuProbe::Unavailable(format!("could not run nvidia-smi: {e}")),
        Ok(Ok(out)) if !out.status.success() => {
            GpuProbe::Unavailable(format!("nvidia-smi exited {}", out.status))
        }
        Ok(Ok(out)) => parse_compute_apps(&String::from_utf8_lossy(&out.stdout), pid),
    }
}

fn parse_compute_apps(output: &str, pid: u32) -> GpuProbe {
    for line in output.lines() {
        let mut cols = line.split(',').map(str::trim);
        let (Some(p), Some(mem)) = (cols.next(), cols.next()) else {
            continue;
        };
        if p.parse::<u32>().ok() == Some(pid) {
            return GpuProbe::Held(mem.parse().unwrap_or(0));
        }
    }
    GpuProbe::NotHeld
}

/// `None` when the X server at `display` accepts a connection (or cannot be
/// checked, e.g. a TCP display); otherwise why not.
fn x11_unreachable(display: &str) -> Option<String> {
    // `:N` or `:N.S` names a local unix socket; `host:N` is TCP, which this
    // cheap probe does not attempt.
    let local = display.strip_prefix(':')?;
    let number: String = local.chars().take_while(char::is_ascii_digit).collect();
    if number.is_empty() {
        return None;
    }
    let socket = format!("/tmp/.X11-unix/X{number}");
    match std::os::unix::net::UnixStream::connect(&socket) {
        Ok(_) => None,
        Err(e) => Some(format!("{socket}: {}", short_io_error(&e))),
    }
}

fn short_io_error(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        std::io::ErrorKind::NotFound => "no such file".into(),
        _ => e.to_string(),
    }
}

fn process_name(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The `grammar_model` check (the id predates S21 and is kept for `doctor
/// --json` consumers): walk the `[format.llm]` ladder the way the formatter
/// resolves it. Ok when the preferred (head) model is installed, Warn when a
/// fallback rung will be used, Fail when no rung is installed.
fn check_formatter_ladder(
    llm: &dictate_fmt::llm::LlmConfig,
    probe: &ollama::OllamaProbe,
) -> DiagnosticCheck {
    const ID: &str = "grammar_model";
    const TITLE: &str = "Formatter model";
    if !llm.enabled {
        return DiagnosticCheck::skipped(ID, TITLE, "the formatting pass is disabled");
    }
    if !probe.reachable {
        return DiagnosticCheck::skipped(
            ID,
            TITLE,
            "Ollama is unreachable, so installed models are unknown",
        );
    }
    let ladder = llm.models.join(" → ");
    let Some(head) = llm.models.first() else {
        return DiagnosticCheck::fail(
            ID,
            TITLE,
            "format.llm.models is empty — the formatting pass has no model to run",
            "list at least one installed model in format.llm.models",
        );
    };
    match llm.models.iter().position(|m| probe.has_model(m)) {
        Some(0) => DiagnosticCheck::ok(ID, TITLE, format!("'{head}' is installed")),
        Some(i) => DiagnosticCheck::warn(
            ID,
            TITLE,
            format!(
                "preferred '{head}' is not installed; falling back to '{}' (ladder: {ladder})",
                llm.models[i]
            ),
            format!("`ollama pull {head}` to use the preferred model"),
        ),
        None => {
            let installed = ollama::describe_installed(&probe.models);
            DiagnosticCheck::fail(
                ID,
                TITLE,
                format!(
                    "no model in the ladder ({ladder}) is installed (installed: {installed}) — the formatting pass fails open on every dictation"
                ),
                format!(
                    "`ollama pull {head}`, or put one of these in format.llm.models: {installed}"
                ),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_smi_output_is_matched_by_pid() {
        let out = "1234, 8100\n5678, 1700\n";
        assert_eq!(parse_compute_apps(out, 5678), GpuProbe::Held(1700));
        assert_eq!(parse_compute_apps(out, 999), GpuProbe::NotHeld);
        assert_eq!(parse_compute_apps("", 1), GpuProbe::NotHeld);
        assert_eq!(parse_compute_apps("garbage\n", 1), GpuProbe::NotHeld);
    }

    #[test]
    fn a_local_display_maps_to_its_socket_and_a_tcp_display_is_not_probed() {
        // Display 9999 has no server.
        assert!(x11_unreachable(":9999").unwrap().contains("X9999"));
        assert!(x11_unreachable(":9999.0").unwrap().contains("X9999"));
        assert_eq!(x11_unreachable("remote.host:0"), None);
        assert_eq!(x11_unreachable(":"), None);
    }

    #[test]
    fn a_program_on_path_is_found_and_a_made_up_one_is_not() {
        assert!(on_path("sh"));
        assert!(!on_path("definitely-not-a-real-program-xyz"));
    }
}
