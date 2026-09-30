//! Model resolution ladder and health.
//!
//! The production failure this exists for: the grammar pass named
//! `qwen3:14b`, the model was removed from Ollama in August, and every
//! dictation since then failed open without anyone noticing. So:
//!
//! - the configured name is a *preference list* resolved against what is
//!   actually installed (`/api/tags`), at startup and after any failure;
//! - the outcome is a [`LlmHealth`] value that `status`/`doctor` can show,
//!   including which preferred models are missing and what is installed;
//! - every change of health is logged once, loudly (`error!` for unusable,
//!   `warn!` for "running on a fallback"), and never repeated per utterance.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::{error, info, warn};

use super::client::{BackendError, ChatBackend, InstalledModel};

/// Whether a formatter (or the LOCAL route) has a model it can use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LlmHealth {
    /// Turned off in configuration; nothing was probed.
    Disabled,
    /// Not probed yet (the daemon has not finished its startup check).
    Unchecked,
    /// A model from the ladder is installed and will be used.
    Ready {
        model: String,
        /// Ladder entries ahead of `model` that are not installed. Non-empty
        /// means the preferred model is missing and a fallback is in use.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        missing_preferred: Vec<String>,
    },
    /// No usable model: Ollama is down, or nothing on the ladder is
    /// installed.
    Unavailable {
        reason: String,
        /// Installed chat models that could be added to the ladder.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        installed_alternatives: Vec<String>,
    },
}

impl LlmHealth {
    /// The model to use, when ready.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        match self {
            Self::Ready { model, .. } => Some(model),
            _ => None,
        }
    }

    /// One line for logs and `doctor`.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Disabled => "disabled".into(),
            Self::Unchecked => "not checked yet".into(),
            Self::Ready {
                model,
                missing_preferred,
            } if missing_preferred.is_empty() => format!("ready ({model})"),
            Self::Ready {
                model,
                missing_preferred,
            } => format!(
                "ready on fallback {model}; preferred not installed: {}",
                missing_preferred.join(", ")
            ),
            Self::Unavailable {
                reason,
                installed_alternatives,
            } if installed_alternatives.is_empty() => format!("unavailable: {reason}"),
            Self::Unavailable {
                reason,
                installed_alternatives,
            } => format!(
                "unavailable: {reason}; installed: {}",
                installed_alternatives.join(", ")
            ),
        }
    }
}

/// Resolve `ladder` against `installed`. Pure; the policy under test.
///
/// A ladder entry matches an installed model by exact name, and an untagged
/// entry (`gemma4`) also matches `gemma4:latest`, which is how Ollama names
/// an untagged pull.
#[must_use]
pub fn resolve_ladder(ladder: &[String], installed: &[InstalledModel]) -> LlmHealth {
    let mut missing = Vec::new();
    for want in ladder {
        let hit = installed.iter().find(|m| {
            m.name == *want || (!want.contains(':') && m.name == format!("{want}:latest"))
        });
        match hit {
            Some(m) => {
                return LlmHealth::Ready {
                    model: m.name.clone(),
                    missing_preferred: missing,
                }
            }
            None => missing.push(want.clone()),
        }
    }
    LlmHealth::Unavailable {
        reason: format!(
            "none of the configured models is installed: {}",
            ladder.join(", ")
        ),
        installed_alternatives: installed
            .iter()
            .filter(|m| !m.is_embedding())
            .map(|m| m.name.clone())
            .collect(),
    }
}

#[derive(Debug)]
struct State {
    health: LlmHealth,
    checked_at: Option<Instant>,
    /// Force a re-probe on next use (after a model-missing failure).
    stale: bool,
    /// Last summary logged, so a state is reported once, not per utterance.
    reported: Option<String>,
}

/// Holds the resolved model and re-resolves on failure.
#[derive(Debug)]
pub struct ModelResolver {
    ladder: Vec<String>,
    /// Minimum time between probes while unavailable, so a stopped Ollama
    /// costs one refused connection per interval rather than per utterance.
    retry_after: Duration,
    /// What to call this resolver in logs ("format.llm", "local").
    label: &'static str,
    state: Mutex<State>,
}

impl ModelResolver {
    /// A resolver for `ladder` (most preferred first).
    #[must_use]
    pub fn new(label: &'static str, ladder: Vec<String>) -> Self {
        Self {
            ladder,
            retry_after: Duration::from_secs(30),
            label,
            state: Mutex::new(State {
                health: LlmHealth::Unchecked,
                checked_at: None,
                stale: false,
                reported: None,
            }),
        }
    }

    /// Override the unavailable back-off (tests).
    #[must_use]
    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = retry_after;
        self
    }

    /// The configured ladder.
    #[must_use]
    pub fn ladder(&self) -> &[String] {
        &self.ladder
    }

    /// Current health, without probing.
    #[must_use]
    pub fn health(&self) -> LlmHealth {
        self.lock().health.clone()
    }

    /// Whether the next use should probe before calling a model: never
    /// probed, marked stale by a failure, or unavailable for longer than the
    /// back-off.
    #[must_use]
    pub fn needs_probe(&self) -> bool {
        let s = self.lock();
        match (&s.health, s.checked_at) {
            (_, None) => true,
            _ if s.stale => true,
            (LlmHealth::Unavailable { .. }, Some(at)) => at.elapsed() >= self.retry_after,
            _ => false,
        }
    }

    /// Probe `/api/tags` and resolve the ladder. Logs a change of health once.
    pub async fn refresh(&self, backend: &dyn ChatBackend) -> LlmHealth {
        let health = match backend.list_models().await {
            Ok(installed) => resolve_ladder(&self.ladder, &installed),
            Err(e) => LlmHealth::Unavailable {
                reason: e.to_string(),
                installed_alternatives: Vec::new(),
            },
        };
        self.set(health.clone());
        health
    }

    /// Probe if [`needs_probe`](Self::needs_probe), then return the model to
    /// use or the reason there is none.
    pub async fn ensure(&self, backend: &dyn ChatBackend) -> Result<String, String> {
        let health = if self.needs_probe() {
            self.refresh(backend).await
        } else {
            self.health()
        };
        match health {
            LlmHealth::Ready { model, .. } => Ok(model),
            other => Err(other.summary()),
        }
    }

    /// Record a failed model call. A missing model or an unreachable server
    /// changes health (and forces a re-probe); other failures do not — a
    /// timeout on one long utterance says nothing about the next one.
    pub fn record_failure(&self, error: &BackendError) {
        match error {
            BackendError::ModelMissing(model) => {
                let mut s = self.lock();
                s.stale = true;
                drop(s);
                self.set_reason_only(format!("model '{model}' disappeared from Ollama"));
            }
            BackendError::Unreachable(detail) => {
                self.set(LlmHealth::Unavailable {
                    reason: format!("ollama unreachable: {detail}"),
                    installed_alternatives: Vec::new(),
                });
            }
            _ => {}
        }
    }

    fn set_reason_only(&self, reason: String) {
        let mut s = self.lock();
        s.health = LlmHealth::Unavailable {
            reason,
            installed_alternatives: Vec::new(),
        };
        let summary = s.health.summary();
        if s.reported.as_deref() != Some(summary.as_str()) {
            error!(target: "dictate_fmt::llm", resolver = self.label, "{summary}");
            s.reported = Some(summary);
        }
    }

    fn set(&self, health: LlmHealth) {
        let mut s = self.lock();
        s.checked_at = Some(Instant::now());
        s.stale = false;
        let summary = health.summary();
        if s.reported.as_deref() != Some(summary.as_str()) {
            match &health {
                LlmHealth::Unavailable { .. } => {
                    error!(target: "dictate_fmt::llm", resolver = self.label, "LLM {summary}");
                }
                LlmHealth::Ready {
                    missing_preferred, ..
                } if !missing_preferred.is_empty() => {
                    warn!(target: "dictate_fmt::llm", resolver = self.label, "LLM {summary}");
                }
                _ => info!(target: "dictate_fmt::llm", resolver = self.label, "LLM {summary}"),
            }
            s.reported = Some(summary);
        }
        s.health = health;
    }

    /// Mark the resolver disabled (config `enabled = false`).
    pub fn set_disabled(&self) {
        let mut s = self.lock();
        s.health = LlmHealth::Disabled;
        s.checked_at = Some(Instant::now());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A poisoned lock only means another thread panicked mid-update;
        // the state is a plain value and still coherent.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(names: &[&str]) -> Vec<InstalledModel> {
        names
            .iter()
            .map(|n| InstalledModel {
                name: (*n).to_string(),
                family: String::new(),
                size: 0,
            })
            .collect()
    }

    fn ladder(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    #[test]
    fn first_installed_entry_wins() {
        let h = resolve_ladder(
            &ladder(&["gemma4:e4b", "gemma4:12b"]),
            &installed(&["gemma4:12b", "gemma4:e4b"]),
        );
        assert_eq!(
            h,
            LlmHealth::Ready {
                model: "gemma4:e4b".into(),
                missing_preferred: vec![]
            }
        );
    }

    #[test]
    fn a_missing_preferred_model_is_named_not_hidden() {
        // Today's machine: the legacy `qwen3:14b` preference is gone.
        let h = resolve_ladder(
            &ladder(&["qwen3:14b", "gemma4:e4b"]),
            &installed(&["gemma4:e4b", "qwen3.6:27b"]),
        );
        assert_eq!(
            h,
            LlmHealth::Ready {
                model: "gemma4:e4b".into(),
                missing_preferred: vec!["qwen3:14b".into()]
            }
        );
        assert!(h.summary().contains("qwen3:14b"));
    }

    #[test]
    fn nothing_installed_lists_alternatives_without_embedding_models() {
        let h = resolve_ladder(
            &ladder(&["qwen3:14b"]),
            &installed(&["qwen3.6:27b", "qwen3-embedding:0.6b", "gemma4:12b"]),
        );
        match h {
            LlmHealth::Unavailable {
                reason,
                installed_alternatives,
            } => {
                assert!(reason.contains("qwen3:14b"));
                assert_eq!(installed_alternatives, ["qwen3.6:27b", "gemma4:12b"]);
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[test]
    fn untagged_entry_matches_latest() {
        let h = resolve_ladder(&ladder(&["llama3"]), &installed(&["llama3:latest"]));
        assert_eq!(h.model(), Some("llama3:latest"));
        // …but a tagged entry does not match a different tag.
        let h = resolve_ladder(&ladder(&["llama3:8b"]), &installed(&["llama3:latest"]));
        assert_eq!(h.model(), None);
    }

    #[test]
    fn health_serializes_for_status_and_doctor() {
        let v = serde_json::to_value(LlmHealth::Unavailable {
            reason: "x".into(),
            installed_alternatives: vec!["a".into()],
        })
        .unwrap();
        assert_eq!(v["state"], "unavailable");
        assert_eq!(v["installed_alternatives"][0], "a");
        let v = serde_json::to_value(LlmHealth::Ready {
            model: "m".into(),
            missing_preferred: vec![],
        })
        .unwrap();
        assert_eq!(v, serde_json::json!({"state": "ready", "model": "m"}));
    }

    #[test]
    fn probe_policy() {
        let r = ModelResolver::new("t", ladder(&["m"])).with_retry_after(Duration::from_secs(3600));
        assert!(r.needs_probe(), "never probed");
        r.set(LlmHealth::Ready {
            model: "m".into(),
            missing_preferred: vec![],
        });
        assert!(!r.needs_probe());
        r.record_failure(&BackendError::Timeout(Duration::from_millis(5)));
        assert!(!r.needs_probe(), "a timeout is not a health change");
        assert_eq!(r.health().model(), Some("m"));
        r.record_failure(&BackendError::ModelMissing("m".into()));
        assert!(r.needs_probe(), "a missing model forces a re-probe");
        assert_eq!(r.health().model(), None);

        r.set(LlmHealth::Unavailable {
            reason: "down".into(),
            installed_alternatives: vec![],
        });
        assert!(!r.needs_probe(), "unavailable backs off");
        let r = ModelResolver::new("t", ladder(&["m"])).with_retry_after(Duration::ZERO);
        r.record_failure(&BackendError::Unreachable("refused".into()));
        assert!(r.needs_probe(), "back-off elapsed");
    }
}
