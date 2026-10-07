//! S21's guarded LLM formatter behind the pipeline's [`Formatter`] port.
//!
//! `dictate_fmt::llm::LlmFormatter` owns the model ladder, masking of
//! protected spans, validators and warm-up; this adapter maps it onto the
//! port the pipeline already speaks and feeds S03's [`HealthTracker`], so a
//! missing or unreachable model is announced once (log + one desktop
//! notification) and shows in `get_status` and `dictate doctor` — the
//! silent six-week failure of the June binary's grammar pass must not recur.

use std::sync::Arc;

use dictate_fmt::llm::{LlmConfig, LlmFormatter, LlmGate, LlmHealth, LlmPlan, LlmRequest};
use dictate_fmt::FormatContext;
use dictate_proto::{AppCategory, FormatterStatus, Tone};

use crate::formatter_health::HealthTracker;
use crate::ports::{BoxFuture, FormatPlan, Formatted, Formatter, StatusNotifier};

/// The production formatter.
pub struct LlmFormatterPort {
    llm: Arc<LlmFormatter>,
    health: HealthTracker,
}

impl LlmFormatterPort {
    /// Build from `[format.llm]`.
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        Self::from_formatter(Arc::new(LlmFormatter::new(config)))
    }

    /// Wrap an existing formatter (tests supply a fake chat backend).
    #[must_use]
    pub fn from_formatter(llm: Arc<LlmFormatter>) -> Self {
        let config = llm.config();
        let head = config.models.first().cloned().unwrap_or_default();
        let health = HealthTracker::new(config.enabled, head);
        Self { llm, health }
    }

    /// Announce failures through `notifier` (once per journey into failure).
    #[must_use]
    pub fn with_notifier(mut self, notifier: Arc<dyn StatusNotifier>) -> Self {
        self.health = self.health.with_notifier(notifier);
        self
    }

    /// The wrapped formatter.
    #[must_use]
    pub fn inner(&self) -> &Arc<LlmFormatter> {
        &self.llm
    }

    fn request(text: &str, ctx: &FormatContext) -> LlmRequest {
        LlmRequest {
            text: text.to_owned(),
            protected: ctx.protected.clone(),
            category: ctx
                .app
                .as_ref()
                .map(|app| app.category.clone())
                .unwrap_or_default(),
            tone: ctx.tone.clone(),
            vocabulary: ctx.vocabulary.clone(),
            language: ctx.language.clone(),
            private: !ctx.persist,
        }
    }

    /// Translate the ladder's view into the tracker's observations.
    fn observe(&self, health: &LlmHealth) {
        match health {
            LlmHealth::Ready { .. } => self.health.observe_ok(),
            LlmHealth::Unavailable {
                reason,
                installed_alternatives,
            } => {
                // The resolver says "none of the configured models is
                // installed" when Ollama answered; anything else means the
                // tag listing itself failed, i.e. Ollama is not reachable.
                if reason.starts_with("none of the configured models") {
                    self.health.observe_model_missing(installed_alternatives);
                } else {
                    self.health.observe_unreachable(reason);
                }
            }
            LlmHealth::Disabled | LlmHealth::Unchecked => {}
        }
    }
}

impl Formatter for LlmFormatterPort {
    fn plan(&self, text: &str, ctx: &FormatContext) -> FormatPlan {
        // The pipeline already applied the session's and the profile's
        // opt-outs (they hold for every formatter); what remains is the
        // session's explicit `Some(true)`, which also overrides `min_words`.
        let gate = LlmGate {
            route: ctx.route.clone(),
            profile_llm_format: None,
            session_format_llm: ctx.format_llm,
        };
        match self.llm.plan(&Self::request(text, ctx), &gate) {
            LlmPlan::Run => FormatPlan::Run,
            LlmPlan::Skip(reason) => FormatPlan::Skip(reason),
        }
    }

    fn format<'a>(&'a self, text: &'a str, ctx: &'a FormatContext) -> BoxFuture<'a, Formatted> {
        Box::pin(async move {
            let outcome = self.llm.format(&Self::request(text, ctx)).await;
            match (&outcome.error, &outcome.validator_rejection) {
                // The model answered; a validator refusing the answer is the
                // pass working as designed, not an unhealthy formatter.
                (_, Some(_)) | (None, None) => self.health.observe_ok(),
                (Some(error), None) => match self.llm.health() {
                    unavailable @ LlmHealth::Unavailable { .. } => self.observe(&unavailable),
                    _ => self.health.observe_error(error),
                },
            }
            Formatted {
                changed: outcome.changed,
                text: outcome.text,
                error: outcome.error,
                duration_s: outcome.duration.as_secs_f64(),
            }
        })
    }

    fn status(&self) -> Option<FormatterStatus> {
        let mut status = self.health.status();
        // Report the model the ladder actually resolved, and say so when it
        // is a fallback for a preferred model that is not installed.
        if let LlmHealth::Ready {
            model,
            missing_preferred,
        } = self.llm.health()
        {
            if !missing_preferred.is_empty() && status.detail.is_none() {
                status.detail = Some(format!(
                    "using fallback '{model}'; preferred not installed: {}",
                    missing_preferred.join(", ")
                ));
            }
            status.model = Some(model);
        }
        Some(status)
    }

    fn probe(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Resolves the ladder against the installed models and, when
            // configured, loads the model so the first dictation is warm.
            let health = self.llm.start().await;
            self.observe(&health);
        })
    }

    fn warm_up(&self, category: &AppCategory, tone: &Tone) {
        let config = self.llm.config();
        if config.enabled && config.warmup_on_record {
            // Deduplicated and never blocking; loads the model while the
            // user is still speaking.
            self.llm
                .warm_up_in_background(category.clone(), tone.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dictate_fmt::llm::client::{BoxFuture as LlmFuture, InstalledModel};
    use dictate_fmt::llm::{BackendError, ChatBackend, ChatRequest, ChatResponse};
    use dictate_proto::{AppContext, FormatterHealth, Route, SkipReason};
    use std::sync::Mutex;
    use std::time::Duration;

    /// A scripted Ollama: installed models, and one reply for every chat.
    struct Fake {
        installed: Result<Vec<&'static str>, String>,
        reply: Mutex<Result<String, BackendError>>,
        chats: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(
            installed: Result<Vec<&'static str>, String>,
            reply: Result<&str, BackendError>,
        ) -> Arc<Self> {
            Arc::new(Self {
                installed,
                reply: Mutex::new(reply.map(str::to_owned)),
                chats: Mutex::new(Vec::new()),
            })
        }
    }

    impl ChatBackend for Fake {
        fn chat<'a>(
            &'a self,
            request: &'a ChatRequest,
        ) -> LlmFuture<'a, Result<ChatResponse, BackendError>> {
            Box::pin(async move {
                let prompt = request
                    .messages
                    .iter()
                    .map(|m| m.content.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.chats.lock().unwrap().push(prompt);
                match &*self.reply.lock().unwrap() {
                    Ok(content) => Ok(ChatResponse {
                        content: content.clone(),
                        done_reason: Some("stop".into()),
                        prompt_eval_count: 0,
                        prompt_eval_cached_count: 0,
                        eval_count: 1,
                        load_ms: 0.0,
                        prompt_eval_ms: 0.0,
                        eval_ms: 1.0,
                    }),
                    Err(e) => Err(e.clone()),
                }
            })
        }

        fn list_models(&self) -> LlmFuture<'_, Result<Vec<InstalledModel>, BackendError>> {
            Box::pin(async move {
                match &self.installed {
                    Ok(names) => Ok(names
                        .iter()
                        .map(|n| InstalledModel {
                            name: (*n).to_string(),
                            family: String::new(),
                            size: 0,
                        })
                        .collect()),
                    Err(e) => Err(BackendError::Unreachable(e.clone())),
                }
            })
        }

        fn load<'a>(
            &'a self,
            _model: &'a str,
            _keep_alive: &'a str,
        ) -> LlmFuture<'a, Result<Duration, BackendError>> {
            Box::pin(async { Ok(Duration::from_millis(1)) })
        }
    }

    fn port(fake: Arc<Fake>) -> LlmFormatterPort {
        let config = LlmConfig {
            enabled: true,
            warmup_on_start: false,
            warmup_on_record: false,
            ..LlmConfig::default()
        };
        LlmFormatterPort::from_formatter(Arc::new(LlmFormatter::with_backend(config, fake)))
    }

    fn ctx(app: &str, category: AppCategory) -> FormatContext {
        FormatContext {
            app: Some(AppContext {
                category,
                ..AppContext::new(app)
            }),
            route: Route::Type,
            ..FormatContext::default()
        }
    }

    #[test]
    fn private_context_reaches_the_llm_request() {
        let mut context = ctx("synthetic-terminal", AppCategory::Terminal);
        context.persist = false;
        assert!(LlmFormatterPort::request("synthetic private words", &context).private);
        context.persist = true;
        assert!(!LlmFormatterPort::request("synthetic public words", &context).private);
    }

    #[tokio::test]
    async fn a_ready_ladder_formats_and_reports_the_resolved_model() {
        let fake = Fake::new(
            Ok(vec!["gemma4:e4b"]),
            Ok("So we should ship the fix today."),
        );
        let port = port(fake.clone());
        port.probe().await;
        let ctx = ctx("slack", AppCategory::Chat);
        let text = "so we should ship the fix today";
        assert_eq!(port.plan(text, &ctx), FormatPlan::Run);
        let out = port.format(text, &ctx).await;
        assert_eq!(out.error, None);
        assert_eq!(out.text, "So we should ship the fix today.");
        let status = port.status().unwrap();
        assert_eq!(status.health, FormatterHealth::Ok);
        assert_eq!(status.model.as_deref(), Some("gemma4:e4b"));
    }

    #[tokio::test]
    async fn a_missing_model_is_reported_as_missing_with_the_alternatives() {
        let fake = Fake::new(Ok(vec!["qwen3.6:27b"]), Ok("unused"));
        let port = port(fake);
        port.probe().await;
        let status = port.status().unwrap();
        assert_eq!(status.health, FormatterHealth::ModelMissing);
        assert!(
            status
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("qwen3.6:27b"),
            "{status:?}"
        );
        // Known-unavailable: the pass is skipped without a connection attempt.
        assert_eq!(
            port.plan(
                "so we should ship the fix today",
                &ctx("slack", AppCategory::Chat)
            ),
            FormatPlan::Skip(SkipReason::DependencyUnavailable)
        );
    }

    #[tokio::test]
    async fn an_unreachable_ollama_is_reported_as_unreachable() {
        let fake = Fake::new(Err("connection refused".into()), Ok("unused"));
        let port = port(fake);
        port.probe().await;
        assert_eq!(port.status().unwrap().health, FormatterHealth::Unreachable);
    }

    #[tokio::test]
    async fn a_validator_rejection_fails_open_without_marking_the_formatter_unhealthy() {
        // The model "answers" the dictation instead of formatting it.
        let fake = Fake::new(
            Ok(vec!["gemma4:e4b"]),
            Ok("Sure! Here is a function that sorts users by last name."),
        );
        let port = port(fake);
        port.probe().await;
        let text = "write a function that sorts users by last name";
        let out = port.format(text, &ctx("slack", AppCategory::Chat)).await;
        assert_eq!(out.text, text, "fail open to the rules output");
        assert!(out.error.is_some());
        assert_eq!(port.status().unwrap().health, FormatterHealth::Ok);
    }

    #[tokio::test]
    async fn protected_ranges_from_the_chain_are_masked_and_restored() {
        let fake = Fake::new(
            Ok(vec!["gemma4:e4b"]),
            // A model that would have mangled the command never sees it.
            Ok("<k1/> for the new login flow, please."),
        );
        let port = port(fake.clone());
        port.probe().await;
        let text = "/create_plan for the new login flow please";
        // One protected byte range: the slash command.
        let command = 0.."/create_plan".len();
        let ctx = FormatContext {
            protected: vec![command],
            ..ctx("ghostty", AppCategory::Terminal)
        };
        let out = port.format(text, &ctx).await;
        assert!(out.text.starts_with("/create_plan"), "{out:?}");
        let prompts = fake.chats.lock().unwrap().join("\n");
        assert!(
            !prompts.contains("/create_plan"),
            "the protected command must never reach the model"
        );
    }

    #[tokio::test]
    async fn a_session_that_insists_overrides_min_words() {
        let fake = Fake::new(Ok(vec!["gemma4:e4b"]), Ok("Ship it."));
        let port = port(fake);
        port.probe().await;
        let short = FormatContext {
            format_llm: Some(true),
            ..ctx("slack", AppCategory::Chat)
        };
        assert_eq!(
            port.plan("ship it", &ctx("slack", AppCategory::Chat)),
            FormatPlan::Skip(SkipReason::BelowMinWords)
        );
        assert_eq!(port.plan("ship it", &short), FormatPlan::Run);
    }

    /// #1069 / #1072: the legacy grammar pass scrubbed every trailing
    /// "thank you." from the model's answer. The LLM path must keep a
    /// dictated one.
    #[tokio::test]
    async fn a_dictated_thank_you_survives_formatting() {
        for (input, reply) in [
            ("i just wanted to thank you", "I just wanted to thank you."),
            (
                "thanks for the report. thank you",
                "Thanks for the report. Thank you.",
            ),
        ] {
            let fake = Fake::new(Ok(vec!["gemma4:e4b"]), Ok(reply));
            let port = port(fake);
            port.probe().await;
            let ctx = ctx("slack", AppCategory::Chat);
            assert_eq!(port.plan(input, &ctx), FormatPlan::Run);
            let out = port.format(input, &ctx).await;
            assert_eq!(out.error, None, "{input}");
            assert_eq!(out.text, reply);
        }
    }
}
