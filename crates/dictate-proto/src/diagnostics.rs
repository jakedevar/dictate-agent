//! Diagnostics: `dictate doctor`'s wire types, and the health fields
//! `get_status` reports.
//!
//! # Why this exists
//!
//! The June binary's grammar pass failed on every dictation for six weeks
//! (`model 'qwen3:14b' not found`) and nobody noticed, because a formatter that
//! fails open is indistinguishable from one that has nothing to fix. Every
//! dependency the daemon leans on therefore gets a named check with a verdict
//! and — crucially — a one-line **fix**, and the two that fail silently
//! (formatter, microphone) are also surfaced in [`Status`](crate::Status).

use serde::{Deserialize, Serialize};

open_str_enum! {
    /// The verdict of one diagnostic check.
    pub enum CheckStatus {
        /// Working as configured.
        Ok => "ok",
        /// Working, but something the user would want to know about (a legacy
        /// daemon holds the signal PID file; the microphone is held open for
        /// pre-roll).
        Warn => "warn",
        /// Broken: a feature the configuration asks for will not work.
        Fail => "fail",
        /// Not evaluated, either because it does not apply to this
        /// configuration (hotkeys disabled) or because a quick run was asked
        /// for.
        Skipped => "skipped",
    }
}

/// One named check.
///
/// `id` is the stable, machine-readable name (`stt_model`, `formatter`, …);
/// `title` and `detail` are for people and may change between releases.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticCheck {
    /// Stable identifier — see `docs/protocol.md` for the vocabulary.
    pub id: String,
    /// Short human title.
    pub title: String,
    /// The verdict.
    pub status: CheckStatus,
    /// What was observed, in one line. Never contains transcript text.
    #[serde(default)]
    pub detail: String,
    /// The one-line remedy, present whenever `status` is `warn` or `fail`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl DiagnosticCheck {
    /// A passing check.
    pub fn ok(id: &str, title: &str, detail: impl Into<String>) -> Self {
        Self::new(id, title, CheckStatus::Ok, detail, None)
    }

    /// A check that passes but deserves attention.
    pub fn warn(
        id: &str,
        title: &str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self::new(id, title, CheckStatus::Warn, detail, Some(fix.into()))
    }

    /// A failing check.
    pub fn fail(
        id: &str,
        title: &str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self::new(id, title, CheckStatus::Fail, detail, Some(fix.into()))
    }

    /// A check that was not evaluated.
    pub fn skipped(id: &str, title: &str, detail: impl Into<String>) -> Self {
        Self::new(id, title, CheckStatus::Skipped, detail, None)
    }

    fn new(
        id: &str,
        title: &str,
        status: CheckStatus,
        detail: impl Into<String>,
        fix: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status,
            detail: detail.into(),
            fix,
        }
    }
}

/// The answer to [`Command::Diagnose`](crate::Command::Diagnose).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DiagnosticsReport {
    /// Every check, in the order a person should read them.
    #[serde(default)]
    pub checks: Vec<DiagnosticCheck>,
}

impl DiagnosticsReport {
    /// Whether nothing failed. Warnings do not fail a report: they describe a
    /// working daemon the user may want to adjust.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        !self.checks.iter().any(|c| c.status == CheckStatus::Fail)
    }

    /// The worst verdict present (`fail` > `warn` > `ok`); `skipped` only when
    /// nothing else was evaluated.
    #[must_use]
    pub fn overall(&self) -> CheckStatus {
        let has = |s: &CheckStatus| self.checks.iter().any(|c| &c.status == s);
        if has(&CheckStatus::Fail) {
            CheckStatus::Fail
        } else if has(&CheckStatus::Warn) {
            CheckStatus::Warn
        } else if has(&CheckStatus::Ok) {
            CheckStatus::Ok
        } else {
            CheckStatus::Skipped
        }
    }

    /// Look a check up by its stable id.
    #[must_use]
    pub fn check(&self, id: &str) -> Option<&DiagnosticCheck> {
        self.checks.iter().find(|c| c.id == id)
    }
}

open_str_enum! {
    /// How the formatting pass is doing, as observed — not as configured.
    pub enum FormatterHealth {
        /// Turned off in configuration; nothing is expected of it.
        Disabled => "disabled",
        /// Enabled but not yet exercised or probed (no observation either way).
        Unchecked => "unchecked",
        /// The last probe or run succeeded.
        Ok => "ok",
        /// The backend answered but does not have the configured model. The
        /// pass fails open on every dictation until this is fixed.
        ModelMissing => "model_missing",
        /// The backend could not be reached.
        Unreachable => "unreachable",
        /// The last run failed for another reason.
        Failing => "failing",
    }
    default = Unchecked;
}

/// The formatting pass's observed health, reported by `get_status`.
///
/// Additive to [`Status`](crate::Status): a formatter that fails open is
/// invisible in every other field, which is exactly how the June binary's
/// broken grammar pass went unnoticed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormatterStatus {
    /// Whether the pass is enabled in configuration.
    #[serde(default)]
    pub enabled: bool,
    /// The model the pass is configured to use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Observed health.
    #[serde(default)]
    pub health: FormatterHealth,
    /// One line of context (the last error, or the installed alternatives).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The microphone's state, reported by `get_status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioStatus {
    /// Whether this daemon may use the microphone at all (`[audio] capture`).
    #[serde(default)]
    pub capture_enabled: bool,
    /// Whether an input device is open **right now**. `true` while idle means
    /// the pre-roll ring is armed and the desktop's microphone indicator is
    /// lit; `pre_roll_ms = 0` keeps this `false` between recordings.
    #[serde(default)]
    pub input_open: bool,
    /// The configured pre-roll, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_roll_ms: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overall_is_the_worst_verdict_and_warnings_do_not_fail_a_report() {
        let mut r = DiagnosticsReport::default();
        assert_eq!(r.overall(), CheckStatus::Skipped);
        r.checks.push(DiagnosticCheck::skipped("hotkeys", "Hotkeys", "disabled"));
        assert_eq!(r.overall(), CheckStatus::Skipped);
        r.checks.push(DiagnosticCheck::ok("daemon", "Daemon", "up"));
        assert_eq!(r.overall(), CheckStatus::Ok);
        r.checks
            .push(DiagnosticCheck::warn("legacy_pid", "PID file", "held", "stop it"));
        assert_eq!(r.overall(), CheckStatus::Warn);
        assert!(r.is_healthy());
        r.checks.push(DiagnosticCheck::fail(
            "formatter",
            "Formatter",
            "model missing",
            "ollama pull x",
        ));
        assert_eq!(r.overall(), CheckStatus::Fail);
        assert!(!r.is_healthy());
    }

    #[test]
    fn warnings_and_failures_always_carry_a_fix() {
        assert!(DiagnosticCheck::warn("a", "A", "d", "f").fix.is_some());
        assert!(DiagnosticCheck::fail("a", "A", "d", "f").fix.is_some());
        assert!(DiagnosticCheck::ok("a", "A", "d").fix.is_none());
    }

    #[test]
    fn checks_are_found_by_stable_id() {
        let r = DiagnosticsReport {
            checks: vec![DiagnosticCheck::ok("stt_model", "Model", "verified")],
        };
        assert!(r.check("stt_model").is_some());
        assert!(r.check("nope").is_none());
    }
}
