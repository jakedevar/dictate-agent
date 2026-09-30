//! `dictate doctor` — is everything the daemon leans on actually working?
//!
//! The daemon runs the real checks (`diagnose`), because it is the process
//! whose environment matters. The CLI contributes the one check only it can
//! make — whether a daemon is reachable at all — and, when there is none, the
//! one thing it can still verify on its own: the speech model file.

use dictate_proto::{CheckStatus, Command, CommandResult, DiagnosticCheck, DiagnosticsReport};

use crate::client::{self, Client};

/// Render a report for a terminal: one line per check, and the fix under every
/// warning and failure.
pub fn render(report: &DiagnosticsReport) -> String {
    let mut out = String::new();
    for check in &report.checks {
        let tag = match check.status {
            CheckStatus::Ok => "  ok  ",
            CheckStatus::Warn => " warn ",
            CheckStatus::Fail => " FAIL ",
            CheckStatus::Skipped | CheckStatus::Unknown(_) => " skip ",
        };
        out.push_str(&format!("[{tag}] {:<24} {}\n", check.title, check.detail));
        if let Some(fix) = &check.fix {
            out.push_str(&format!("         fix: {fix}\n"));
        }
    }
    let failed = report
        .checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .count();
    let warned = report
        .checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .count();
    out.push('\n');
    out.push_str(&match (failed, warned) {
        (0, 0) => "everything checks out".to_string(),
        (0, w) => format!("healthy, with {w} warning(s)"),
        (f, w) => format!("{f} problem(s) need fixing ({w} warning(s))"),
    });
    out.push('\n');
    out
}

/// The check the CLI makes for the daemon it is talking to.
pub async fn daemon_check(client: &mut Client) -> DiagnosticCheck {
    let socket = client::socket_path();
    let detail = match client.try_request(Command::GetStatus).await {
        Ok(Ok(CommandResult::Status(s))) => format!(
            "{} {} (protocol v{}) at {}{}",
            s.daemon.name,
            s.daemon.version,
            s.daemon.protocol_version,
            socket.display(),
            s.daemon
                .pid
                .map(|p| format!(", pid {p}"))
                .unwrap_or_default()
        ),
        _ => format!(
            "{} {} at {}",
            client.hello.server.name,
            client.hello.server.version,
            socket.display()
        ),
    };
    DiagnosticCheck::ok("daemon", "Daemon", detail)
}

/// The report for a daemon that could not be reached: that fact, plus the model
/// file check the CLI can make on its own.
pub fn unreachable_report(error: &anyhow::Error) -> DiagnosticsReport {
    let mut checks = vec![DiagnosticCheck::fail(
        "daemon",
        "Daemon",
        format!("{error:#}"),
        "start it: `systemctl --user start dictated`, or run `dictated` in a terminal to see why it will not start",
    )];
    checks.push(local_model_check());
    DiagnosticsReport { checks }
}

/// The model file at the default location, checked by presence and size (the
/// daemon's own check also hashes it; without a daemon that is too slow to be
/// a reasonable default here).
fn local_model_check() -> DiagnosticCheck {
    let config = dictate_stt::WhisperConfig::default();
    let path = dictate_stt::config::expand_tilde(&config.model_path);
    let spec = dictate_stt::catalog_model(&config.model);
    match (std::fs::metadata(&path), spec) {
        (Ok(meta), Some(spec)) if meta.len() == spec.bytes => DiagnosticCheck::ok(
            "stt_model",
            "Speech model",
            format!(
                "{} present at {} (size matches the catalog; the daemon's own check hashes it)",
                spec.id,
                path.display()
            ),
        ),
        (Ok(meta), Some(spec)) => DiagnosticCheck::fail(
            "stt_model",
            "Speech model",
            format!(
                "{} is {} bytes; the pinned {} is {} bytes",
                path.display(),
                meta.len(),
                spec.id,
                spec.bytes
            ),
            format!("run `dictate model pull {}`", spec.id),
        ),
        (Ok(_), None) => DiagnosticCheck::ok(
            "stt_model",
            "Speech model",
            format!("present at {}", path.display()),
        ),
        (Err(_), _) => DiagnosticCheck::fail(
            "stt_model",
            "Speech model",
            format!("no model at {}", path.display()),
            format!("run `dictate model pull {}`", config.model),
        ),
    }
}

/// Whether a report should make `dictate doctor` exit non-zero.
pub fn is_failure(report: &DiagnosticsReport) -> bool {
    !report.is_healthy()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DiagnosticsReport {
        DiagnosticsReport {
            checks: vec![
                DiagnosticCheck::ok("daemon", "Daemon", "dictated 0.2.0"),
                DiagnosticCheck::warn(
                    "legacy_pid",
                    "Legacy PID",
                    "held by dictate-agent",
                    "stop it",
                ),
                DiagnosticCheck::fail(
                    "formatter",
                    "Formatting pass",
                    "model missing",
                    "ollama pull x",
                ),
                DiagnosticCheck::skipped("hotkeys", "Global hotkeys", "disabled"),
            ],
        }
    }

    #[test]
    fn every_warning_and_failure_prints_its_fix_and_nothing_else_does() {
        let text = render(&sample());
        assert!(text.contains("fix: stop it"));
        assert!(text.contains("fix: ollama pull x"));
        assert_eq!(text.matches("fix:").count(), 2);
        assert!(
            text.contains("[ FAIL ]") && text.contains("[ warn ]") && text.contains("[  ok  ]")
        );
        assert!(text.contains("1 problem(s) need fixing (1 warning(s))"));
    }

    #[test]
    fn a_clean_report_says_so() {
        let text = render(&DiagnosticsReport {
            checks: vec![DiagnosticCheck::ok("a", "A", "fine")],
        });
        assert!(text.contains("everything checks out"));
    }

    #[test]
    fn warnings_alone_do_not_fail_the_command() {
        let mut r = sample();
        r.checks.retain(|c| c.status != CheckStatus::Fail);
        assert!(!is_failure(&r));
        assert!(is_failure(&sample()));
    }

    #[test]
    fn an_unreachable_daemon_is_a_failure_with_a_way_forward() {
        let report =
            unreachable_report(&anyhow::anyhow!("no daemon listening on /x/dictated.sock"));
        assert!(is_failure(&report));
        let daemon = report.check("daemon").unwrap();
        assert!(daemon.detail.contains("no daemon listening"));
        assert!(daemon.fix.as_deref().unwrap().contains("dictated"));
        // The model check still runs, so the report is useful with no daemon.
        assert!(report.check("stt_model").is_some());
    }
}
