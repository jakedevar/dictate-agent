//! Observed health of the formatting pass — the antidote to silent failure.
//!
//! The formatting pass **fails open**: when the LLM cannot be used it hands the
//! text back unchanged. That is the right behavior for the user in the moment
//! and the worst possible behavior for the user's awareness — the June binary's
//! grammar pass failed on every dictation for six weeks (`model 'qwen3:14b' not
//! found`) and nothing anywhere said so.
//!
//! [`HealthTracker`] turns each observation (a startup probe, a real run) into
//! an observable state, and makes a *change for the worse* loud exactly once:
//! a WARN in the log and a single desktop notification. It does not spam: a
//! formatter that stays broken is reported once, not once per dictation, and
//! recovery is logged when it happens. `get_status` and `dictate doctor` read
//! the same state.
//!
//! It is deliberately independent of *which* formatter it watches so the LLM
//! layer that replaces today's grammar pass (S21) can reuse it unchanged.

use std::sync::{Arc, Mutex};

use dictate_proto::{FormatterHealth, FormatterStatus};
use tracing::{info, warn};

use crate::ollama;
use crate::ollama_host::OllamaHost;
use crate::ports::{Notice, StatusNotifier};

struct State {
    health: FormatterHealth,
    detail: Option<String>,
}

/// Tracks and announces a formatter's health.
pub struct HealthTracker {
    enabled: bool,
    model: String,
    state: Mutex<State>,
    notifier: Option<Arc<dyn StatusNotifier>>,
    /// Where else the model might be installed, and the endpoint asked (#5725).
    stores: Option<(OllamaHost, String)>,
}

impl HealthTracker {
    /// A tracker for a formatter configured with `model`.
    #[must_use]
    pub fn new(enabled: bool, model: impl Into<String>) -> Self {
        Self {
            enabled,
            model: model.into(),
            state: Mutex::new(State {
                health: if enabled {
                    FormatterHealth::Unchecked
                } else {
                    FormatterHealth::Disabled
                },
                detail: None,
            }),
            notifier: None,
            stores: None,
        }
    }

    /// When the model is missing from the server at `endpoint`, look in the
    /// other Ollama model stores of `host` first: a model that exists in a
    /// store the server does not read must not be answered with `ollama pull`.
    #[must_use]
    pub fn with_model_stores(mut self, host: OllamaHost, endpoint: impl Into<String>) -> Self {
        self.stores = Some((host, endpoint.into()));
        self
    }

    /// Raise a desktop notification through `notifier` on the first failure.
    #[must_use]
    pub fn with_notifier(mut self, notifier: Arc<dyn StatusNotifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    /// The current observed state, for `get_status`.
    #[must_use]
    pub fn status(&self) -> FormatterStatus {
        let state = self.state.lock().expect("formatter health poisoned");
        FormatterStatus {
            enabled: self.enabled,
            model: Some(self.model.clone()),
            health: state.health.clone(),
            detail: state.detail.clone(),
        }
    }

    /// A real run (or probe) succeeded.
    pub fn observe_ok(&self) {
        if !self.enabled {
            return;
        }
        let mut state = self.state.lock().expect("formatter health poisoned");
        let was_bad = is_bad(&state.health);
        state.health = FormatterHealth::Ok;
        state.detail = None;
        drop(state);
        if was_bad {
            info!(model = %self.model, "formatter recovered");
        }
    }

    /// A real run failed with `error`. Classifies it and announces a change for
    /// the worse.
    pub fn observe_error(&self, error: &str) {
        if !self.enabled {
            return;
        }
        // The length-ratio guard rejecting an answer proves the server *did*
        // answer: the pass is healthy, the model just said something unusable.
        if error.starts_with("Length ratio") {
            self.observe_ok();
            return;
        }
        let health = if ollama::is_model_missing_error(error) {
            FormatterHealth::ModelMissing
        } else if ollama::is_unreachable_error(error) {
            FormatterHealth::Unreachable
        } else {
            FormatterHealth::Failing
        };
        let detail = match health {
            FormatterHealth::ModelMissing => format!(
                "model '{}' is not installed in Ollama — run `dictate doctor`",
                self.model
            ),
            _ => error.to_string(),
        };
        self.transition_bad(health, detail);
    }

    /// A startup probe found Ollama up but `model` absent.
    pub fn observe_model_missing(&self, installed: &[String]) {
        if !self.enabled {
            return;
        }
        let elsewhere = self
            .stores
            .as_ref()
            .and_then(|(host, endpoint)| host.wrong_store_explanation(&self.model, endpoint));
        let detail = match elsewhere {
            Some(why) => why,
            None => format!(
                "model '{}' is not installed in Ollama (installed: {}); \
                 `ollama pull {}` or list an installed model in [format.llm] models",
                self.model,
                ollama::describe_installed(&ollama::chat_models(installed)),
                self.model
            ),
        };
        self.transition_bad(FormatterHealth::ModelMissing, detail);
    }

    /// A startup probe could not reach Ollama.
    pub fn observe_unreachable(&self, error: &str) {
        if !self.enabled {
            return;
        }
        self.transition_bad(
            FormatterHealth::Unreachable,
            format!("Ollama is not reachable: {error}"),
        );
    }

    fn transition_bad(&self, health: FormatterHealth, detail: String) {
        let mut state = self.state.lock().expect("formatter health poisoned");
        let newly_bad = !is_bad(&state.health);
        let changed = state.health != health;
        state.health = health.clone();
        state.detail = Some(detail.clone());
        drop(state);
        if !changed {
            return;
        }
        warn!(
            model = %self.model,
            health = %health,
            "formatter is unhealthy — dictations will be typed unformatted: {detail}"
        );
        // One popup per journey into failure, not per dictation.
        if newly_bad {
            if let Some(notifier) = &self.notifier {
                notifier.notify(Notice::Error(match health {
                    FormatterHealth::ModelMissing => format!(
                        "Formatter model '{}' is not installed — dictations are unformatted. Run: dictate doctor",
                        self.model
                    ),
                    FormatterHealth::Unreachable => {
                        "Ollama is not reachable — dictations are unformatted. Run: dictate doctor"
                            .to_string()
                    }
                    _ => "The formatting pass is failing — dictations are unformatted. Run: dictate doctor"
                        .to_string(),
                }));
            }
        }
    }
}

fn is_bad(health: &FormatterHealth) -> bool {
    matches!(
        health,
        FormatterHealth::ModelMissing | FormatterHealth::Unreachable | FormatterHealth::Failing
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::mock::RecordingNotifier;

    fn tracker() -> (HealthTracker, Arc<RecordingNotifier>) {
        let notifier = Arc::new(RecordingNotifier::default());
        (
            HealthTracker::new(true, "qwen3:14b").with_notifier(notifier.clone()),
            notifier,
        )
    }

    fn errors(n: &RecordingNotifier) -> usize {
        n.notices()
            .iter()
            .filter(|n| matches!(n, Notice::Error(_)))
            .count()
    }

    #[test]
    fn a_fresh_tracker_has_observed_nothing_rather_than_claiming_health() {
        let (t, _) = tracker();
        assert_eq!(t.status().health, FormatterHealth::Unchecked);
        assert_eq!(
            HealthTracker::new(false, "m").status().health,
            FormatterHealth::Disabled
        );
    }

    #[test]
    fn the_june_failure_is_classified_and_raises_exactly_one_notification() {
        let (t, n) = tracker();
        t.observe_error("model 'qwen3:14b' not found");
        let s = t.status();
        assert_eq!(s.health, FormatterHealth::ModelMissing);
        assert!(s.detail.unwrap().contains("dictate doctor"));
        assert_eq!(errors(&n), 1);

        // Six weeks of identical failures must not become six weeks of popups.
        for _ in 0..50 {
            t.observe_error("model 'qwen3:14b' not found");
        }
        assert_eq!(errors(&n), 1, "the same failure must be announced once");
    }

    #[test]
    fn a_dead_server_and_a_missing_model_are_different_states() {
        let (t, _) = tracker();
        t.observe_error("error sending request for url (http://localhost:11434/api/generate)");
        assert_eq!(t.status().health, FormatterHealth::Unreachable);
        t.observe_error("model 'x' not found");
        assert_eq!(t.status().health, FormatterHealth::ModelMissing);
        t.observe_error("Empty response from grammar model");
        assert_eq!(t.status().health, FormatterHealth::Failing);
    }

    #[test]
    fn a_rejected_answer_is_not_a_broken_formatter() {
        let (t, n) = tracker();
        t.observe_error("Length ratio 2.10 outside 0.5-1.5 range");
        assert_eq!(t.status().health, FormatterHealth::Ok);
        assert_eq!(errors(&n), 0, "one odd answer must not raise a popup");
    }

    #[test]
    fn a_worse_state_after_a_bad_one_updates_health_without_a_second_popup() {
        let (t, n) = tracker();
        t.observe_error("connection refused");
        t.observe_error("model 'x' not found");
        assert_eq!(errors(&n), 1);
    }

    #[test]
    fn recovery_clears_the_state_and_a_later_failure_notifies_again() {
        let (t, n) = tracker();
        t.observe_error("model 'x' not found");
        t.observe_ok();
        assert_eq!(t.status().health, FormatterHealth::Ok);
        assert_eq!(t.status().detail, None);
        t.observe_error("model 'x' not found");
        assert_eq!(errors(&n), 2, "a fresh journey into failure is news again");
    }

    #[test]
    fn a_startup_probe_lists_what_is_installed() {
        let (t, n) = tracker();
        t.observe_model_missing(&["gemma4:12b".into(), "qwen3.6:27b".into()]);
        let detail = t.status().detail.unwrap();
        assert!(detail.contains("gemma4:12b, qwen3.6:27b"), "{detail}");
        assert!(detail.contains("ollama pull qwen3:14b"), "{detail}");
        assert_eq!(errors(&n), 1);
    }

    #[test]
    fn a_model_in_another_store_is_not_answered_with_a_pull() {
        use crate::ollama_host::testing::Scratch;
        use crate::ollama_host::UnitScope;
        let dir = Scratch::new("health");
        dir.manifest("sys-store", "registry.ollama.ai/library/qwen3/14b");
        dir.write("units/ollama.service", "[Service]\n");
        let host = OllamaHost::default()
            .with_unit_dir(dir.path("units"), UnitScope::System)
            .with_store_root(dir.path("sys-store"));
        let t = HealthTracker::new(true, "qwen3:14b").with_model_stores(host, "http://127.0.0.1:1");
        t.observe_model_missing(&[]);
        let detail = t.status().detail.unwrap();
        assert!(!detail.contains("ollama pull"), "{detail}");
        assert!(detail.contains("sudo systemctl restart ollama"), "{detail}");
        assert!(detail.contains("sys-store"), "{detail}");
    }

    #[test]
    fn a_disabled_formatter_never_complains_about_anything() {
        let n = Arc::new(RecordingNotifier::default());
        let t = HealthTracker::new(false, "m").with_notifier(n.clone());
        t.observe_error("model 'm' not found");
        t.observe_model_missing(&[]);
        t.observe_unreachable("down");
        assert_eq!(t.status().health, FormatterHealth::Disabled);
        assert_eq!(errors(&n), 0);
    }
}
