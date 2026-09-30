//! S21 — the formatting LLM layer.
//!
//! Takes the deterministic rules output and asks a local model (Ollama) to
//! make it read as if the speaker had typed it carefully: fillers and false
//! starts gone, self-corrections resolved, punctuation and structure right,
//! register matched to the app. With zero tolerance for changing meaning,
//! answering what was dictated, or corrupting technical tokens, and it never
//! fails silently:
//!
//! - **Fail-open.** Every failure — Ollama down, model missing, timeout,
//!   malformed reply, a validator rejecting the output — returns the input
//!   unchanged, with the reason in [`LlmOutcome::error`] and, for a
//!   rejection, the validator in [`LlmOutcome::validator_rejection`].
//! - **Protected spans** are detached or masked before the model sees them
//!   and verified on the way back ([`protect`]).
//! - **Validators** ([`validate`]) guard meaning and leakage.
//! - **Health** ([`LlmHealth`]) resolves a model ladder against what is
//!   installed and reports a missing model loudly, once ([`resolve`]).
//! - **Skip rules** ([`LlmFormatter::plan`]) decide up front, so the
//!   pipeline records `Skipped{reason}` instead of a fabricated `Ran`.
//!
//! The seam is independent of S20's types: the integrator maps
//! `FormatContext` and the protected document onto [`LlmRequest`].

pub mod chunk;
pub mod client;
pub mod config;
pub mod eval;
pub mod prompt;
pub mod protect;
pub mod resolve;
pub mod validate;

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dictate_proto::{AppCategory, Route, SkipReason, Tone};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

pub use client::{BackendError, ChatBackend, ChatRequest, ChatResponse, HttpBackend};
pub use config::{CategoryPolicy, LlmConfig, LlmConfigLoad, Style};
pub use protect::MaskStyle;
pub use resolve::{LlmHealth, ModelResolver};
pub use validate::{Rejection, Thresholds, Validator};

use client::ChatOptions;
use prompt::PromptSpec;

/// One formatting request: the rules output plus the context the prompt and
/// validators need.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LlmRequest {
    /// The deterministic rules output.
    pub text: String,
    /// Byte ranges of `text` that must survive verbatim.
    pub protected: Vec<Range<usize>>,
    pub category: AppCategory,
    pub tone: Tone,
    /// In-scope dictionary terms (S22); spelled exactly if spoken.
    pub vocabulary: Vec<String>,
    /// Detected or pinned STT language (BCP-47).
    pub language: Option<String>,
}

impl LlmRequest {
    /// A request for `text` with default context.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

/// Session-level inputs to the skip decision that are not part of the text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LlmGate {
    /// Route decided on the rules output, before this pass.
    pub route: Route,
    /// The matched app profile's `llm_format` (S23), if it sets one.
    pub profile_llm_format: Option<bool>,
    /// `SessionOptions.format_llm`: `Some(false)` always skips,
    /// `Some(true)` overrides the profile, category and `min_words` defaults.
    pub session_format_llm: Option<bool>,
}

/// Whether the pass will run, decided before it is attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmPlan {
    Run,
    Skip(SkipReason),
}

/// Result of a formatting pass.
///
/// Invariant: `error.is_some()` ⇔ nothing from the model was applied and
/// `text` is the input byte for byte. A long input formatted in chunks can
/// apply some chunks and fall back on others; then `error` is `None`,
/// `segments_applied < segments`, and `validator_rejection` names the first
/// rejection.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmOutcome {
    pub text: String,
    pub changed: bool,
    /// The model that ran, if one was reached.
    pub model: Option<String>,
    pub error: Option<String>,
    pub validator_rejection: Option<Rejection>,
    pub duration: Duration,
    pub segments: usize,
    pub segments_applied: usize,
}

/// Per-chunk detail for the eval harness. Holds text; never persisted by the
/// daemon.
#[derive(Debug, Clone, Default)]
pub struct SegmentTrace {
    /// What the model saw (masked, prefix detached).
    pub masked_input: String,
    /// What the model said, cleaned but before validation.
    pub raw_output: Option<String>,
    pub rejection: Option<Rejection>,
    pub error: Option<String>,
    pub response: Option<ChatResponse>,
    pub latency: Duration,
    /// Words the model was sent for this chunk.
    pub words: usize,
}

/// Every chunk of one pass.
#[derive(Debug, Clone, Default)]
pub struct LlmTrace {
    pub segments: Vec<SegmentTrace>,
}

/// The formatter. Cheap to share behind an `Arc`.
pub struct LlmFormatter {
    config: LlmConfig,
    backend: Arc<dyn ChatBackend>,
    resolver: ModelResolver,
    mask: MaskStyle,
    thresholds: Thresholds,
    warming: AtomicBool,
    last_warm: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for LlmFormatter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmFormatter")
            .field("health", &self.resolver.health())
            .field("mask", &self.mask)
            .finish_non_exhaustive()
    }
}

/// The mask style chosen by the S21 evaluation (research note §Masking):
/// `<k1/>` had zero span rejections over 316 corpus + stress cases and the
/// best stress-set pass rate, and inline XML placeholder tags are the
/// convention models are trained to carry through (machine translation).
pub const DEFAULT_MASK: MaskStyle = MaskStyle::XmlTag;

/// A tiny synthetic dictation used to prime the prompt cache on warm-up.
const PRIME_TEXT: &str = "okay thanks";

impl LlmFormatter {
    /// A formatter talking to the configured Ollama over HTTP.
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        let backend = Arc::new(HttpBackend::new(&config.host));
        Self::with_backend(config, backend)
    }

    /// A formatter over any backend (tests, recorded eval).
    #[must_use]
    pub fn with_backend(config: LlmConfig, backend: Arc<dyn ChatBackend>) -> Self {
        let resolver = ModelResolver::new("format.llm", config.models.clone());
        if !config.enabled {
            resolver.set_disabled();
        }
        Self {
            config,
            backend,
            resolver,
            mask: DEFAULT_MASK,
            thresholds: Thresholds::default(),
            warming: AtomicBool::new(false),
            last_warm: Mutex::new(None),
        }
    }

    /// Use a different placeholder style (evaluation sweep).
    #[must_use]
    pub fn with_mask_style(mut self, mask: MaskStyle) -> Self {
        self.mask = mask;
        self
    }

    /// Use different validator thresholds (evaluation sweep).
    #[must_use]
    pub fn with_thresholds(mut self, thresholds: Thresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    /// Override the resolver's back-off while unavailable (tests).
    #[must_use]
    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        let resolver = ModelResolver::new("format.llm", self.config.models.clone())
            .with_retry_after(retry_after);
        if !self.config.enabled {
            resolver.set_disabled();
        }
        self.resolver = resolver;
        self
    }

    #[must_use]
    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// Current health, without probing. For `status` and `doctor`.
    #[must_use]
    pub fn health(&self) -> LlmHealth {
        self.resolver.health()
    }

    /// Probe Ollama and resolve the ladder now (daemon start, `doctor`).
    pub async fn refresh(&self) -> LlmHealth {
        if !self.config.enabled {
            return LlmHealth::Disabled;
        }
        self.resolver.refresh(self.backend.as_ref()).await
    }

    /// Decide whether the pass runs, without calling a model.
    ///
    /// Order: global switch, route, session opt-out, profile/category
    /// defaults (overridable by `Some(true)`), words to format, hard length
    /// limit, and finally known unavailability.
    #[must_use]
    pub fn plan(&self, req: &LlmRequest, gate: &LlmGate) -> LlmPlan {
        if !self.config.enabled {
            return LlmPlan::Skip(SkipReason::Disabled);
        }
        if gate.route != Route::Type {
            return LlmPlan::Skip(SkipReason::RouteNotEligible);
        }
        let forced = gate.session_format_llm == Some(true);
        if gate.session_format_llm == Some(false) {
            return LlmPlan::Skip(SkipReason::Disabled);
        }
        if !forced
            && (gate.profile_llm_format == Some(false)
                || !self.config.categories.get(&req.category).enabled)
        {
            return LlmPlan::Skip(SkipReason::Disabled);
        }
        // Words the model would actually see; an utterance that is all
        // protected spans ("/compact") has nothing to format. An invalid
        // range fails later, in `format`, where the error is reported.
        let spans = protect::normalize_spans(&req.text, &req.protected, self.config.protect_fallback)
            .unwrap_or_default();
        let words = protect::unprotected_words(&req.text, &spans);
        if words == 0 || (!forced && words < self.config.min_words) {
            return LlmPlan::Skip(SkipReason::BelowMinWords);
        }
        if req.text.split_whitespace().count() > self.config.chunking.max_words {
            return LlmPlan::Skip(SkipReason::TooLong);
        }
        if matches!(self.resolver.health(), LlmHealth::Unavailable { .. })
            && !self.resolver.needs_probe()
        {
            return LlmPlan::Skip(SkipReason::DependencyUnavailable);
        }
        LlmPlan::Run
    }

    /// Format, failing open.
    pub async fn format(&self, req: &LlmRequest) -> LlmOutcome {
        self.format_traced(req).await.0
    }

    /// Format, failing open, and return per-chunk detail for evaluation.
    pub async fn format_traced(&self, req: &LlmRequest) -> (LlmOutcome, LlmTrace) {
        let start = Instant::now();
        let fail = |error: String, model: Option<String>| LlmOutcome {
            text: req.text.clone(),
            changed: false,
            model,
            error: Some(error),
            validator_rejection: None,
            duration: start.elapsed(),
            segments: 0,
            segments_applied: 0,
        };

        if !self.config.enabled {
            return (fail("LLM formatting is disabled".into(), None), LlmTrace::default());
        }
        let spans = match protect::normalize_spans(
            &req.text,
            &req.protected,
            self.config.protect_fallback,
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!("LLM pass not attempted: {e}");
                return (fail(e.to_string(), None), LlmTrace::default());
            }
        };
        let model = match self.resolver.ensure(self.backend.as_ref()).await {
            Ok(m) => m,
            Err(reason) => {
                return (
                    fail(format!("no LLM model available: {reason}"), None),
                    LlmTrace::default(),
                )
            }
        };

        let total_words = req.text.split_whitespace().count();
        let ranges = if total_words > self.config.chunking.max_single_words {
            chunk::split(&req.text, &spans, self.config.chunking.chunk_words)
        } else {
            vec![0..req.text.len()]
        };
        let deadline = start + self.config.timeout.for_words(total_words);
        let policy = self.config.categories.get(&req.category).clone();

        let jobs: Vec<SegmentJob> = ranges
            .iter()
            .map(|r| {
                let seg_spans = spans
                    .iter()
                    .filter(|s| s.start >= r.start && s.end <= r.end)
                    .map(|s| s.start - r.start..s.end - r.start)
                    .collect();
                SegmentJob {
                    text: req.text[r.clone()].to_string(),
                    spans: seg_spans,
                }
            })
            .collect();

        let ctx = Arc::new(SegmentCtx {
            backend: self.backend.clone(),
            model: model.clone(),
            policy,
            tone: req.tone.clone(),
            vocabulary: req.vocabulary.clone(),
            language: req.language.clone(),
            mask: self.mask,
            thresholds: self.thresholds.clone(),
            keep_alive: self.config.keep_alive.clone(),
            temperature: self.config.temperature,
            timeout: self.config.timeout.clone(),
            deadline,
        });
        let results = run_segments(ctx, jobs, self.config.chunking.concurrency).await;

        let mut text = String::with_capacity(req.text.len() + 32);
        let mut applied = 0;
        let mut first_error: Option<String> = None;
        let mut first_rejection: Option<Rejection> = None;
        let mut trace = LlmTrace::default();
        for (result, range) in results.into_iter().zip(&ranges) {
            match &result.formatted {
                Some(t) => {
                    text.push_str(t);
                    applied += 1;
                }
                None => text.push_str(&req.text[range.clone()]),
            }
            if let Some(e) = &result.backend_error {
                self.resolver.record_failure(e);
            }
            if first_rejection.is_none() {
                first_rejection = result.trace.rejection.clone();
            }
            if first_error.is_none() {
                first_error = result.trace.error.clone();
            }
            trace.segments.push(result.trace);
        }

        let segments = ranges.len();
        let error = (applied == 0).then(|| {
            first_error.unwrap_or_else(|| "LLM produced no usable output".to_string())
        });
        let text = if applied == 0 { req.text.clone() } else { text };
        let changed = text != req.text;
        let outcome = LlmOutcome {
            changed,
            model: Some(model),
            error,
            validator_rejection: first_rejection,
            duration: start.elapsed(),
            segments,
            segments_applied: applied,
            text,
        };
        match (&outcome.error, &outcome.validator_rejection) {
            (Some(e), _) => warn!(
                model = outcome.model.as_deref().unwrap_or(""),
                ms = outcome.duration.as_millis() as u64,
                "LLM pass failed open: {e}"
            ),
            (None, Some(r)) => warn!(
                applied,
                segments,
                "LLM pass partially applied; chunk rejected by {}",
                r.validator.as_str()
            ),
            (None, None) => debug!(
                ms = outcome.duration.as_millis() as u64,
                changed, "LLM pass applied"
            ),
        }
        (outcome, trace)
    }

    /// Load the model now, and optionally prime Ollama's prompt cache with
    /// the prompt prefix for `prime` so the first real request only
    /// evaluates its own turn. Returns the wall time spent.
    ///
    /// # Errors
    ///
    /// The reason no model could be loaded.
    pub async fn warm_up(&self, prime: Option<(&AppCategory, &Tone)>) -> Result<Duration, String> {
        if !self.config.enabled {
            return Err("LLM formatting is disabled".into());
        }
        let started = Instant::now();
        let model = self.resolver.ensure(self.backend.as_ref()).await?;
        if let Err(e) = self.backend.load(&model, &self.config.keep_alive).await {
            self.resolver.record_failure(&e);
            return Err(e.to_string());
        }
        if let Some((category, tone)) = prime {
            let policy = self.config.categories.get(category);
            let spec = PromptSpec {
                policy,
                tone,
                mask: self.mask,
                vocabulary: &[],
                language: None,
            };
            let request = ChatRequest {
                model: model.clone(),
                messages: prompt::build_messages(&spec, PRIME_TEXT),
                stream: false,
                think: false,
                keep_alive: self.config.keep_alive.clone(),
                options: ChatOptions {
                    temperature: self.config.temperature,
                    num_predict: 1,
                    stop: prompt::stop_sequences(),
                    seed: SEED,
                },
            };
            if let Err(e) = self.backend.chat(&request).await {
                debug!("prompt-cache priming failed (harmless): {e}");
            }
        }
        if let Ok(mut last) = self.last_warm.lock() {
            *last = Some(Instant::now());
        }
        Ok(started.elapsed())
    }

    /// Warm up without waiting, for the pipeline to call when recording
    /// starts. At most one warm-up is in flight; a no-op when disabled, when
    /// `warmup_on_record` is off, or outside a tokio runtime.
    pub fn warm_up_in_background(self: &Arc<Self>, category: AppCategory, tone: Tone) {
        if !self.config.enabled || !self.config.warmup_on_record {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.warming.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = Arc::clone(self);
        handle.spawn(async move {
            match this.warm_up(Some((&category, &tone))).await {
                Ok(d) => debug!(ms = d.as_millis() as u64, "LLM warm-up done"),
                Err(e) => debug!("LLM warm-up skipped: {e}"),
            }
            this.warming.store(false, Ordering::Release);
        });
    }

    /// Daemon-start hook: resolve the ladder, then load the model if
    /// `warmup_on_start`. Logs the outcome once.
    pub async fn start(&self) -> LlmHealth {
        let health = self.refresh().await;
        if self.config.warmup_on_start && health.model().is_some() {
            match self.warm_up(Some((&AppCategory::Terminal, &Tone::Neutral))).await {
                Ok(d) => info!(ms = d.as_millis() as u64, "LLM model loaded"),
                Err(e) => warn!("LLM warm-up at start failed: {e}"),
            }
        }
        self.health()
    }

    /// Unload the resolved model (`keep_alive: 0`).
    ///
    /// # Errors
    ///
    /// If there is no resolved model or the call fails.
    pub async fn unload(&self) -> Result<(), String> {
        let model = self
            .resolver
            .health()
            .model()
            .map(str::to_string)
            .ok_or_else(|| "no model resolved".to_string())?;
        self.backend
            .load(&model, "0")
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Fixed sampling seed, so recordings are reproducible.
const SEED: i32 = 7;

struct SegmentJob {
    text: String,
    spans: Vec<Range<usize>>,
}

struct SegmentCtx {
    backend: Arc<dyn ChatBackend>,
    model: String,
    policy: CategoryPolicy,
    tone: Tone,
    vocabulary: Vec<String>,
    language: Option<String>,
    mask: MaskStyle,
    thresholds: Thresholds,
    keep_alive: String,
    temperature: f32,
    timeout: config::TimeoutPolicy,
    deadline: Instant,
}

struct SegmentResult {
    /// Full replacement for the segment (whitespace preserved), if applied.
    formatted: Option<String>,
    backend_error: Option<BackendError>,
    trace: SegmentTrace,
}

/// Run chunks with at most `concurrency` in flight, preserving order. Tasks
/// live in a `JoinSet`, so dropping this future (session cancelled) aborts
/// them.
async fn run_segments(
    ctx: Arc<SegmentCtx>,
    jobs: Vec<SegmentJob>,
    concurrency: usize,
) -> Vec<SegmentResult> {
    if jobs.len() == 1 {
        let job = jobs.into_iter().next().expect("one job");
        return vec![format_segment(&ctx, job).await];
    }
    let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut set = tokio::task::JoinSet::new();
    let n = jobs.len();
    for (i, job) in jobs.into_iter().enumerate() {
        let ctx = ctx.clone();
        let semaphore = semaphore.clone();
        set.spawn(async move {
            let _permit = semaphore.acquire_owned().await;
            (i, format_segment(&ctx, job).await)
        });
    }
    let mut out: Vec<Option<SegmentResult>> = (0..n).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((i, r)) => out[i] = Some(r),
            Err(e) => warn!("LLM chunk task failed: {e}"),
        }
    }
    out.into_iter()
        .map(|r| {
            r.unwrap_or_else(|| SegmentResult {
                formatted: None,
                backend_error: None,
                trace: SegmentTrace {
                    error: Some("chunk task failed".into()),
                    ..SegmentTrace::default()
                },
            })
        })
        .collect()
}

async fn format_segment(ctx: &SegmentCtx, job: SegmentJob) -> SegmentResult {
    let started = Instant::now();
    let (lead, core, trail) = chunk::trim_parts(&job.text);
    let core_offset = lead.len();
    let spans: Vec<Range<usize>> = job
        .spans
        .iter()
        .filter(|s| s.start >= core_offset && s.end <= core_offset + core.len())
        .map(|s| s.start - core_offset..s.end - core_offset)
        .collect();

    let mut trace = SegmentTrace {
        words: core.split_whitespace().count(),
        ..SegmentTrace::default()
    };
    let unchanged = |mut trace: SegmentTrace, error: Option<String>, rejection: Option<Rejection>, backend_error| {
        trace.latency = started.elapsed();
        trace.error = error;
        trace.rejection = rejection;
        SegmentResult {
            formatted: None,
            backend_error,
            trace,
        }
    };

    let masked = match protect::mask(core, &spans, ctx.mask) {
        Ok(m) => m,
        Err(e) => return unchanged(trace, Some(e.to_string()), None, None),
    };
    trace.masked_input = masked.body.clone();
    if masked.body.split_whitespace().next().is_none() {
        // Nothing but protected text: nothing to do, and nothing failed.
        trace.latency = started.elapsed();
        return SegmentResult {
            formatted: Some(job.text),
            backend_error: None,
            trace,
        };
    }

    let remaining = ctx.deadline.saturating_duration_since(Instant::now());
    let timeout = ctx
        .timeout
        .for_words(masked.body.split_whitespace().count())
        .min(remaining);
    if timeout.is_zero() {
        return unchanged(trace, Some("pass deadline reached before this chunk".into()), None, None);
    }

    let spec = PromptSpec {
        policy: &ctx.policy,
        tone: &ctx.tone,
        mask: ctx.mask,
        vocabulary: &ctx.vocabulary,
        language: ctx.language.as_deref(),
    };
    let request = ChatRequest {
        model: ctx.model.clone(),
        messages: prompt::build_messages(&spec, &masked.body),
        stream: false,
        think: false,
        keep_alive: ctx.keep_alive.clone(),
        options: ChatOptions {
            temperature: ctx.temperature,
            num_predict: prompt::num_predict(&masked.body),
            stop: prompt::stop_sequences(),
            seed: SEED,
        },
    };

    let response = match tokio::time::timeout(timeout, ctx.backend.chat(&request)).await {
        Err(_) => {
            let e = BackendError::Timeout(timeout);
            return unchanged(trace, Some(e.to_string()), None, Some(e));
        }
        Ok(Err(e)) => return unchanged(trace, Some(e.to_string()), None, Some(e)),
        Ok(Ok(r)) => r,
    };

    let output = clean_output(&response.content, &masked.body);
    trace.raw_output = Some(output.clone());
    let truncated = response.done_reason.as_deref() == Some("length");
    trace.response = Some(response);

    let reject = |trace, r: Rejection| {
        let msg = format!("validator rejected output: {r}");
        unchanged(trace, Some(msg), Some(r), None)
    };
    if truncated {
        return reject(
            trace,
            Rejection::new(Validator::Truncated, "generation hit num_predict"),
        );
    }
    if output.trim().is_empty() {
        return reject(trace, Rejection::new(Validator::EmptyOutput, "no text returned"));
    }
    let restored = match masked.restore(&output) {
        Ok(r) => r,
        Err(e) => return reject(trace, Rejection::new(Validator::ProtectedSpans, e.to_string())),
    };
    if let Err(r) = validate::validate(&validate::Check {
        input: &masked.body,
        output: &output,
        policy: &ctx.policy,
        vocabulary: &ctx.vocabulary,
        mask: ctx.mask,
        thresholds: &ctx.thresholds,
    }) {
        return reject(trace, r);
    }

    trace.latency = started.elapsed();
    SegmentResult {
        formatted: Some(format!("{lead}{restored}{trail}")),
        backend_error: None,
        trace,
    }
}

/// Deterministic repairs of harmless wrapping, before validation: a
/// `<think>` block, echoed delimiters around the whole reply, and one pair of
/// quotes around the whole reply when the input was not quoted.
fn clean_output(raw: &str, input: &str) -> String {
    let mut s = raw.replace("\r\n", "\n");
    if let Some(rest) = s.trim_start().strip_prefix("<think>") {
        if let Some(end) = rest.find("</think>") {
            s = rest[end + "</think>".len()..].to_string();
        }
    }
    let mut t = s.trim();
    if let Some(rest) = t.strip_prefix(prompt::OPEN) {
        t = rest.trim();
    }
    if let Some(rest) = t.strip_suffix(prompt::CLOSE) {
        t = rest.trim();
    }
    for (open, close) in [('"', '"'), ('“', '”')] {
        let input = input.trim();
        let wrapped_in = input.starts_with(open) && input.ends_with(close);
        if !wrapped_in && t.len() >= 2 && t.starts_with(open) && t.ends_with(close) {
            let inner = &t[open.len_utf8()..t.len() - close.len_utf8()];
            if !inner.contains(open) && !inner.contains(close) {
                t = inner.trim();
            }
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_output_repairs_only_whole_wrapping() {
        assert_eq!(clean_output("<think>x</think>\nHi.", "hi"), "Hi.");
        assert_eq!(clean_output("<dictation>\nHi.\n</dictation>", "hi"), "Hi.");
        assert_eq!(clean_output("\"Hi there.\"", "hi there"), "Hi there.");
        // Quotes that were dictated, or that are inside the text, stay.
        assert_eq!(clean_output("\"Hi\" he said.", "hi he said"), "\"Hi\" he said.");
        assert_eq!(clean_output("\"Hi.\"", "\"hi\""), "\"Hi.\"");
        assert_eq!(clean_output("  Hi.\r\n", "hi"), "Hi.");
    }
}
