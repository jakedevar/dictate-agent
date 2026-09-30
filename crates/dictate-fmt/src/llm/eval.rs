//! Eval harness: corpus, per-case scoring, aggregate report, and the
//! backends that record and replay model outputs.
//!
//! Two tiers share this code:
//!
//! - **Recorded (CI).** `tests/llm_eval_recorded.rs` replays committed model
//!   outputs through the real masking, validators, restore and scoring with no
//!   network. A request whose fingerprint has no recording — because a prompt,
//!   example, option or mask changed — is a failure: re-record with
//!   `just eval-llm --record`.
//! - **Live.** `examples/eval_llm.rs` (feature `eval-live`) runs the corpus
//!   against a real Ollama, writes a JSON report, and can re-record.
//!
//! The corpus is synthetic (the repository is public): see
//! `tests/eval/README.md` for the schema and authoring rules.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dictate_proto::{AppCategory, Route, Tone};
use serde::{Deserialize, Serialize};

use super::client::{
    BackendError, BoxFuture, ChatBackend, ChatRequest, ChatResponse, InstalledModel,
};
use super::validate::{self, levenshtein, Validator};
use super::{LlmFormatter, LlmGate, LlmOutcome, LlmPlan, LlmRequest, LlmTrace};

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

/// One eval case. `input` is what the LLM layer receives: the rules output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Case {
    pub id: String,
    pub category: AppCategory,
    #[serde(default)]
    pub tone: Tone,
    pub input: String,
    /// Span texts that must survive verbatim, in order of appearance.
    #[serde(default)]
    pub protected: Vec<String>,
    pub expected: String,
    /// Substrings (case-sensitive) the final text must contain.
    #[serde(default)]
    pub must_keep: Vec<String>,
    /// Substrings (case-insensitive) the final text must not contain — the
    /// answer to a dictated question, an executed instruction, …
    #[serde(default)]
    pub must_not_contain: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub vocabulary: Vec<String>,
}

impl Case {
    /// Byte ranges of `protected`, located left to right.
    ///
    /// # Errors
    ///
    /// If a span text is not found after the previous one.
    pub fn protected_ranges(&self) -> Result<Vec<Range<usize>>, String> {
        let mut pos = 0;
        let mut out = Vec::with_capacity(self.protected.len());
        for p in &self.protected {
            let at = self.input[pos..]
                .find(p.as_str())
                .ok_or_else(|| format!("{}: protected {p:?} not found in input", self.id))?;
            out.push(pos + at..pos + at + p.len());
            pos += at + p.len();
        }
        Ok(out)
    }

    /// The formatter request for this case.
    ///
    /// # Errors
    ///
    /// See [`protected_ranges`](Self::protected_ranges).
    pub fn request(&self) -> Result<LlmRequest, String> {
        Ok(LlmRequest {
            text: self.input.clone(),
            protected: self.protected_ranges()?,
            category: self.category.clone(),
            tone: self.tone.clone(),
            vocabulary: self.vocabulary.clone(),
            language: None,
        })
    }

    #[must_use]
    pub fn words(&self) -> usize {
        self.input.split_whitespace().count()
    }
}

/// Parse JSONL (blank lines ignored).
///
/// # Errors
///
/// The first malformed line, with its number.
pub fn parse_corpus(text: &str) -> Result<Vec<Case>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1)))
        .collect()
}

/// Read and parse a corpus file.
///
/// # Errors
///
/// I/O or parse errors.
pub fn load_corpus(path: &Path) -> Result<Vec<Case>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_corpus(&text)
}

/// Authoring mistakes that would make a case meaningless.
#[must_use]
pub fn lint_corpus(cases: &[Case]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for c in cases {
        if !ids.insert(c.id.as_str()) {
            problems.push(format!("{}: duplicate id", c.id));
        }
        if !c.category.is_known() {
            problems.push(format!("{}: unknown category", c.id));
        }
        if !c.tone.is_known() {
            problems.push(format!("{}: unknown tone", c.id));
        }
        if c.expected.trim().is_empty() {
            problems.push(format!("{}: empty expected", c.id));
        }
        if let Err(e) = c.protected_ranges() {
            problems.push(e);
        }
        for p in &c.protected {
            if c.expected.matches(p.as_str()).count() != c.input.matches(p.as_str()).count() {
                problems.push(format!("{}: expected does not keep protected {p:?}", c.id));
            }
        }
        for k in &c.must_keep {
            if !c.expected.contains(k.as_str()) {
                problems.push(format!("{}: expected lacks must_keep {k:?}", c.id));
            }
        }
        for n in &c.must_not_contain {
            let n = n.to_lowercase();
            if c.input.to_lowercase().contains(&n) || c.expected.to_lowercase().contains(&n) {
                problems.push(format!("{}: must_not_contain {n:?} is in input/expected", c.id));
            }
        }
    }
    problems
}

/// Length bucket, chosen around Jake's p50 (17 words) and p90 (53 words).
#[must_use]
pub fn bucket(words: usize) -> &'static str {
    match words {
        0..=10 => "01-10",
        11..=25 => "11-25",
        26..=40 => "26-40",
        41..=70 => "41-70",
        _ => "71+",
    }
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/// Minimum word similarity to `expected` for a pass (see the research note:
/// one word of slack in a ten-word sentence, two in a twenty-word one).
pub const PASS_SIMILARITY: f64 = 0.85;

/// Scored result of one case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    pub id: String,
    pub category: String,
    pub tone: String,
    pub tags: Vec<String>,
    pub words: usize,
    pub bucket: String,
    /// Number of protected spans in the case.
    pub protected: usize,
    /// Final text after fail-open.
    pub output: String,
    pub changed: bool,
    /// Why the pass did not run, if it was skipped.
    pub skipped: Option<String>,
    pub error: Option<String>,
    pub rejected_by: Option<String>,
    /// Cleaned model outputs, one per chunk (synthetic corpus only).
    pub raw_outputs: Vec<String>,
    /// Model wall time for the case (sum over chunks).
    pub latency_ms: f64,
    pub eval_tokens: u64,
    pub spans_preserved: bool,
    /// Final text leaks (must be zero).
    pub leaked: bool,
    /// The model tried to leak and a validator caught it.
    pub raw_leak: bool,
    pub similarity: f64,
    pub exact: bool,
    pub pass: bool,
    /// Whether the input itself (rules-only) would have passed.
    pub baseline_pass: bool,
    pub fail_reasons: Vec<String>,
}

fn sim_words(s: &str) -> Vec<String> {
    validate::words_for_scoring(s)
}

fn first_alpha_is_upper(s: &str) -> Option<bool> {
    s.chars()
        .find(|c| c.is_alphabetic())
        .map(char::is_uppercase)
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Rubric checks of `text` as a final output for `case`. Returns
/// (pass, similarity, exact, spans_preserved, leaked, reasons).
fn judge(case: &Case, text: &str) -> (bool, f64, bool, bool, bool, Vec<String>) {
    let mut reasons = Vec::new();
    let spans_preserved = case
        .protected
        .iter()
        .all(|p| text.matches(p.as_str()).count() == case.input.matches(p.as_str()).count());
    if !spans_preserved {
        reasons.push("protected span altered".to_string());
    }
    let lower = text.to_lowercase();
    let mut leaked = case
        .must_not_contain
        .iter()
        .any(|n| lower.contains(&n.to_lowercase()));
    if let Some(v) = validate::leakage(&case.input, text) {
        leaked = true;
        reasons.push(format!("leakage: {}", v.as_str()));
    }
    if leaked {
        reasons.push("leaked".to_string());
    }
    for k in &case.must_keep {
        if !text.contains(k.as_str()) {
            reasons.push(format!("missing {k:?}"));
        }
    }
    let (a, b) = (sim_words(text), sim_words(&case.expected));
    let similarity = 1.0 - levenshtein(&a, &b) as f64 / a.len().max(b.len()).max(1) as f64;
    if similarity < PASS_SIMILARITY {
        reasons.push(format!("similarity {similarity:.2}"));
    }
    if text.contains('?') != case.expected.contains('?') {
        reasons.push("question mark mismatch".to_string());
    }
    if text.trim().contains('\n') != case.expected.trim().contains('\n') {
        reasons.push("line structure mismatch".to_string());
    }
    if first_alpha_is_upper(text) != first_alpha_is_upper(&case.expected) {
        reasons.push("initial capitalization mismatch".to_string());
    }
    let exact = normalize_ws(text) == normalize_ws(&case.expected);
    (reasons.is_empty(), similarity, exact, spans_preserved, leaked, reasons)
}

/// Score one case from the pass's outcome.
#[must_use]
pub fn score_case(
    case: &Case,
    skipped: Option<String>,
    outcome: Option<&LlmOutcome>,
    trace: Option<&LlmTrace>,
    latency_ms: f64,
) -> CaseResult {
    let output = outcome.map_or_else(|| case.input.clone(), |o| o.text.clone());
    let (pass, similarity, exact, spans_preserved, leaked, fail_reasons) = judge(case, &output);
    let (baseline_pass, ..) = judge(case, &case.input);
    let segments = trace.map(|t| t.segments.as_slice()).unwrap_or_default();
    let raw_outputs: Vec<String> = segments.iter().filter_map(|s| s.raw_output.clone()).collect();
    let raw_leak = segments
        .iter()
        .any(|s| s.rejection.as_ref().is_some_and(|r| r.validator.is_leakage()))
        || raw_outputs.iter().any(|r| {
            let r = r.to_lowercase();
            case.must_not_contain.iter().any(|n| r.contains(&n.to_lowercase()))
        });
    CaseResult {
        id: case.id.clone(),
        category: case.category.to_string(),
        tone: case.tone.to_string(),
        tags: case.tags.clone(),
        words: case.words(),
        bucket: bucket(case.words()).to_string(),
        protected: case.protected.len(),
        changed: output != case.input,
        output,
        skipped,
        error: outcome.and_then(|o| o.error.clone()),
        rejected_by: outcome
            .and_then(|o| o.validator_rejection.as_ref())
            .map(|r| r.validator.as_str().to_string()),
        raw_outputs,
        latency_ms,
        eval_tokens: segments
            .iter()
            .filter_map(|s| s.response.as_ref().map(|r| r.eval_count))
            .sum(),
        spans_preserved,
        leaked,
        raw_leak,
        similarity,
        exact,
        pass,
        baseline_pass,
        fail_reasons,
    }
}

/// Run every case through `formatter`, the way the pipeline would: plan
/// first (Type route, no overrides), format only when it runs.
///
/// `latency_of` supplies the recorded latency for a case in the replay tier;
/// `None` uses the measured wall time.
pub async fn run_corpus(
    formatter: &LlmFormatter,
    cases: &[Case],
    latency_of: Option<&dyn Fn(&str) -> f64>,
    mut on_case: impl FnMut(&Case, &CaseResult),
) -> Vec<CaseResult> {
    let gate = LlmGate {
        route: Route::Type,
        ..LlmGate::default()
    };
    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        let request = match case.request() {
            Ok(r) => r,
            Err(e) => {
                let mut r = score_case(case, Some("invalid case".into()), None, None, 0.0);
                r.fail_reasons.push(e);
                r.pass = false;
                results.push(r);
                continue;
            }
        };
        let result = match formatter.plan(&request, &gate) {
            LlmPlan::Skip(reason) => score_case(case, Some(reason.to_string()), None, None, 0.0),
            LlmPlan::Run => {
                let started = Instant::now();
                let (outcome, trace) = formatter.format_traced(&request).await;
                let latency = latency_of.map_or_else(
                    || started.elapsed().as_secs_f64() * 1000.0,
                    |f| f(&case.id),
                );
                score_case(case, None, Some(&outcome), Some(&trace), latency)
            }
        };
        on_case(case, &result);
        results.push(result);
    }
    results
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LatencyStats {
    pub n: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

impl LatencyStats {
    #[must_use]
    pub fn of(values: &[f64]) -> Self {
        let mut v: Vec<f64> = values.to_vec();
        v.sort_by(f64::total_cmp);
        Self {
            n: v.len(),
            p50_ms: percentile(&v, 50.0),
            p95_ms: percentile(&v, 95.0),
            max_ms: v.last().copied().unwrap_or(0.0),
        }
    }
}

/// Nearest-rank percentile of sorted values.
#[must_use]
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Rate {
    pub n: usize,
    pub pass: usize,
    pub pass_rate: f64,
}

/// Aggregate over a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub model: String,
    pub prompt_version: String,
    pub mask_style: String,
    pub cases: usize,
    pub pass_rate: f64,
    pub exact_rate: f64,
    /// Rules-only pass rate on the same rubric: what the LLM layer adds.
    pub baseline_pass_rate: f64,
    /// Fraction of cases with protected spans whose spans all survived.
    pub span_preservation: f64,
    /// Final outputs that leak. Must be 0.
    pub leaked: usize,
    /// Model outputs that tried to leak and were caught.
    pub raw_leak_attempts: usize,
    /// Cases where the pass ran but nothing was applied.
    pub fallback_rate: f64,
    pub rejection_rate: f64,
    pub rejections: BTreeMap<String, usize>,
    pub errors: BTreeMap<String, usize>,
    pub skipped: BTreeMap<String, usize>,
    pub latency: LatencyStats,
    pub latency_by_bucket: BTreeMap<String, LatencyStats>,
    pub by_category: BTreeMap<String, Rate>,
    pub by_tag: BTreeMap<String, Rate>,
    pub failures: Vec<String>,
}

fn error_kind(e: &str) -> String {
    if e.starts_with("validator rejected") {
        "validator".into()
    } else if e.contains("timed out") {
        "timeout".into()
    } else if e.contains("unreachable") {
        "unreachable".into()
    } else if e.contains("not found") {
        "model_missing".into()
    } else if e.contains("malformed") {
        "malformed".into()
    } else if e.contains("no recording") {
        "no_recording".into()
    } else {
        "other".into()
    }
}

/// Build the aggregate report.
#[must_use]
pub fn report(model: &str, mask_style: &str, results: &[CaseResult]) -> Report {
    let n = results.len().max(1) as f64;
    let ran: Vec<&CaseResult> = results.iter().filter(|r| r.skipped.is_none()).collect();
    let ran_n = ran.len().max(1) as f64;
    let with_spans: Vec<&CaseResult> = results.iter().filter(|r| r.protected > 0).collect();
    let mut rejections = BTreeMap::new();
    let mut errors = BTreeMap::new();
    let mut skipped = BTreeMap::new();
    for r in results {
        if let Some(v) = &r.rejected_by {
            *rejections.entry(v.clone()).or_insert(0) += 1;
        }
        if let Some(e) = &r.error {
            *errors.entry(error_kind(e)).or_insert(0) += 1;
        }
        if let Some(s) = &r.skipped {
            *skipped.entry(s.clone()).or_insert(0) += 1;
        }
    }
    let rate = |rs: &[&CaseResult]| {
        let pass = rs.iter().filter(|r| r.pass).count();
        Rate {
            n: rs.len(),
            pass,
            pass_rate: pass as f64 / rs.len().max(1) as f64,
        }
    };
    let mut by_category: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
    let mut by_tag: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
    let mut by_bucket: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for r in results {
        by_category.entry(r.category.clone()).or_default().push(r);
        for t in &r.tags {
            by_tag.entry(t.clone()).or_default().push(r);
        }
    }
    for r in &ran {
        by_bucket.entry(r.bucket.clone()).or_default().push(r.latency_ms);
    }
    let all_latency: Vec<f64> = ran.iter().map(|r| r.latency_ms).collect();
    Report {
        model: model.to_string(),
        prompt_version: super::prompt::PROMPT_VERSION.to_string(),
        mask_style: mask_style.to_string(),
        cases: results.len(),
        pass_rate: results.iter().filter(|r| r.pass).count() as f64 / n,
        exact_rate: results.iter().filter(|r| r.exact).count() as f64 / n,
        baseline_pass_rate: results.iter().filter(|r| r.baseline_pass).count() as f64 / n,
        span_preservation: with_spans.iter().filter(|r| r.spans_preserved).count() as f64
            / with_spans.len().max(1) as f64,
        leaked: results.iter().filter(|r| r.leaked).count(),
        raw_leak_attempts: results.iter().filter(|r| r.raw_leak).count(),
        fallback_rate: ran.iter().filter(|r| r.error.is_some()).count() as f64 / ran_n,
        rejection_rate: ran.iter().filter(|r| r.rejected_by.is_some()).count() as f64 / ran_n,
        rejections,
        errors,
        skipped,
        latency: LatencyStats::of(&all_latency),
        latency_by_bucket: by_bucket
            .into_iter()
            .map(|(k, v)| (k, LatencyStats::of(&v)))
            .collect(),
        by_category: by_category
            .into_iter()
            .map(|(k, v)| (k, rate(&v)))
            .collect(),
        by_tag: by_tag.into_iter().map(|(k, v)| (k, rate(&v))).collect(),
        failures: results
            .iter()
            .filter(|r| !r.pass)
            .map(|r| format!("{}: {}", r.id, r.fail_reasons.join("; ")))
            .collect(),
    }
}

impl Report {
    /// Human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut s = String::new();
        let pct = |x: f64| format!("{:.1}%", x * 100.0);
        s.push_str(&format!(
            "model {} · prompt {} · mask {} · {} cases\n",
            self.model, self.prompt_version, self.mask_style, self.cases
        ));
        s.push_str(&format!(
            "pass {} (rules-only baseline {}) · exact {} · spans preserved {} · leaked {} (caught attempts {})\n",
            pct(self.pass_rate),
            pct(self.baseline_pass_rate),
            pct(self.exact_rate),
            pct(self.span_preservation),
            self.leaked,
            self.raw_leak_attempts
        ));
        s.push_str(&format!(
            "fallback {} · rejection {} {:?} · errors {:?} · skipped {:?}\n",
            pct(self.fallback_rate),
            pct(self.rejection_rate),
            self.rejections,
            self.errors,
            self.skipped
        ));
        s.push_str(&format!(
            "latency all: n={} p50={:.0}ms p95={:.0}ms max={:.0}ms\n",
            self.latency.n, self.latency.p50_ms, self.latency.p95_ms, self.latency.max_ms
        ));
        for (b, l) in &self.latency_by_bucket {
            s.push_str(&format!(
                "  words {b:>5}: n={:<3} p50={:>4.0}ms p95={:>4.0}ms max={:>4.0}ms\n",
                l.n, l.p50_ms, l.p95_ms, l.max_ms
            ));
        }
        s.push_str("by category:");
        for (c, r) in &self.by_category {
            s.push_str(&format!(" {c} {}/{}", r.pass, r.n));
        }
        s.push('\n');
        s
    }
}

// ---------------------------------------------------------------------------
// Recording and replay
// ---------------------------------------------------------------------------

/// One recorded model reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    /// [`request_key`] of the request that produced it.
    pub key: String,
    pub case_id: String,
    pub model: String,
    pub content: String,
    #[serde(default)]
    pub done_reason: Option<String>,
    #[serde(default)]
    pub eval_count: u64,
    #[serde(default)]
    pub prompt_eval_count: u64,
    /// Wall time of the live call.
    pub latency_ms: f64,
}

/// FNV-1a 64 — stable across Rust versions, unlike `DefaultHasher`.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Fingerprint of everything that determines the model's reply: model,
/// messages (system prompt, examples, dictation), and sampling options.
/// `keep_alive` is excluded — it does not affect output.
#[must_use]
pub fn request_key(req: &ChatRequest) -> String {
    #[derive(Serialize)]
    struct Keyed<'a> {
        model: &'a str,
        messages: &'a [super::client::ChatMessage],
        think: bool,
        options: &'a super::client::ChatOptions,
    }
    let json = serde_json::to_string(&Keyed {
        model: &req.model,
        messages: &req.messages,
        think: req.think,
        options: &req.options,
    })
    .unwrap_or_default();
    format!("{:016x}", fnv1a64(json.as_bytes()))
}

/// Recordings file for `model` under `dir`.
#[must_use]
pub fn recordings_path(dir: &Path, model: &str) -> std::path::PathBuf {
    let name: String = model
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    dir.join(format!("{name}.jsonl"))
}

/// Load recordings (JSONL).
///
/// # Errors
///
/// I/O or parse errors.
pub fn load_recordings(path: &Path) -> Result<Vec<Recording>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1)))
        .collect()
}

/// Write recordings (JSONL), sorted by case id for reviewable diffs.
///
/// # Errors
///
/// I/O errors.
pub fn save_recordings(path: &Path, recordings: &[Recording]) -> Result<(), String> {
    let mut sorted = recordings.to_vec();
    sorted.sort_by(|a, b| a.case_id.cmp(&b.case_id).then(a.key.cmp(&b.key)));
    sorted.dedup_by(|a, b| a.key == b.key);
    let mut out = String::new();
    for r in &sorted {
        out.push_str(&serde_json::to_string(r).map_err(|e| e.to_string())?);
        out.push('\n');
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

/// Replays recorded replies by request fingerprint. A miss is an error the
/// test reports as prompt drift.
#[derive(Debug)]
pub struct ReplayBackend {
    installed: Vec<InstalledModel>,
    by_key: HashMap<String, Recording>,
    misses: Mutex<Vec<String>>,
}

impl ReplayBackend {
    /// Replay `recordings`, with `model` reported as the only installed one.
    #[must_use]
    pub fn new(model: &str, recordings: Vec<Recording>) -> Self {
        Self {
            installed: vec![InstalledModel {
                name: model.to_string(),
                family: String::new(),
                size: 0,
            }],
            by_key: recordings.into_iter().map(|r| (r.key.clone(), r)).collect(),
            misses: Mutex::new(Vec::new()),
        }
    }

    /// Requests that had no recording.
    #[must_use]
    pub fn misses(&self) -> Vec<String> {
        self.misses.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// Recorded latency per case id (sum over chunks).
    #[must_use]
    pub fn latency_by_case(&self) -> HashMap<String, f64> {
        let mut out = HashMap::new();
        for r in self.by_key.values() {
            *out.entry(r.case_id.clone()).or_insert(0.0) += r.latency_ms;
        }
        out
    }
}

impl ChatBackend for ReplayBackend {
    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> BoxFuture<'a, Result<ChatResponse, BackendError>> {
        Box::pin(async move {
            let key = request_key(request);
            match self.by_key.get(&key) {
                Some(r) => Ok(ChatResponse {
                    content: r.content.clone(),
                    done_reason: r.done_reason.clone(),
                    eval_count: r.eval_count,
                    prompt_eval_count: r.prompt_eval_count,
                    ..ChatResponse::default()
                }),
                None => {
                    if let Ok(mut m) = self.misses.lock() {
                        m.push(key.clone());
                    }
                    Err(BackendError::Http {
                        status: 0,
                        message: format!(
                            "no recording for request {key} (prompt, options or mask changed? \
                             re-record with `just eval-llm --record`)"
                        ),
                    })
                }
            }
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<InstalledModel>, BackendError>> {
        Box::pin(async move { Ok(self.installed.clone()) })
    }

    fn load<'a>(
        &'a self,
        _model: &'a str,
        _keep_alive: &'a str,
    ) -> BoxFuture<'a, Result<Duration, BackendError>> {
        Box::pin(async move { Ok(Duration::ZERO) })
    }
}

/// Wraps a live backend and records every chat reply against the current
/// case id.
pub struct RecordingBackend {
    inner: Arc<dyn ChatBackend>,
    case_id: Mutex<String>,
    recordings: Mutex<Vec<Recording>>,
}

impl RecordingBackend {
    #[must_use]
    pub fn new(inner: Arc<dyn ChatBackend>) -> Self {
        Self {
            inner,
            case_id: Mutex::new(String::new()),
            recordings: Mutex::new(Vec::new()),
        }
    }

    /// Attribute subsequent calls to `case_id`.
    pub fn set_case(&self, case_id: &str) {
        if let Ok(mut c) = self.case_id.lock() {
            *c = case_id.to_string();
        }
    }

    /// Everything recorded so far.
    #[must_use]
    pub fn recordings(&self) -> Vec<Recording> {
        self.recordings.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

impl ChatBackend for RecordingBackend {
    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> BoxFuture<'a, Result<ChatResponse, BackendError>> {
        Box::pin(async move {
            let started = Instant::now();
            let result = self.inner.chat(request).await;
            if let Ok(r) = &result {
                let case_id = self.case_id.lock().map(|c| c.clone()).unwrap_or_default();
                if let Ok(mut recs) = self.recordings.lock() {
                    recs.push(Recording {
                        key: request_key(request),
                        case_id,
                        model: request.model.clone(),
                        content: r.content.clone(),
                        done_reason: r.done_reason.clone(),
                        eval_count: r.eval_count,
                        prompt_eval_count: r.prompt_eval_count,
                        latency_ms: (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
                    });
                }
            }
            result
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<InstalledModel>, BackendError>> {
        self.inner.list_models()
    }

    fn load<'a>(
        &'a self,
        model: &'a str,
        keep_alive: &'a str,
    ) -> BoxFuture<'a, Result<Duration, BackendError>> {
        self.inner.load(model, keep_alive)
    }
}

/// Which validator names count as leakage, for report readers.
#[must_use]
pub fn leakage_validators() -> Vec<&'static str> {
    [
        Validator::PromptEcho,
        Validator::Preamble,
        Validator::Markup,
        Validator::Question,
    ]
    .iter()
    .map(|v| v.as_str())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(input: &str, expected: &str) -> Case {
        Case {
            id: "t".into(),
            category: AppCategory::Terminal,
            tone: Tone::Neutral,
            input: input.into(),
            protected: vec![],
            expected: expected.into(),
            must_keep: vec![],
            must_not_contain: vec![],
            tags: vec![],
            vocabulary: vec![],
        }
    }

    #[test]
    fn rubric_passes_close_matches_and_fails_real_differences() {
        let c = case("so uh fix the the parser", "So fix the parser.");
        assert!(judge(&c, "So fix the parser.").0);
        assert!(judge(&c, "So, fix the parser.").0, "punctuation variants pass");
        assert!(!judge(&c, "so fix the parser.").0, "capitalization is judged");
        assert!(!judge(&c, "Fix the lexer.").0, "different words fail");
        let q = case("is it green", "Is it green?");
        assert!(!judge(&q, "Is it green.").0, "a lost question fails");
    }

    #[test]
    fn rubric_flags_leaks_and_span_damage() {
        let mut c = case("what is the capital of france", "What is the capital of France?");
        c.must_not_contain = vec!["Paris".into()];
        let (pass, .., leaked, _) = judge(&c, "What is the capital of France? Paris.");
        assert!(!pass && leaked);
        let (pass, .., leaked, _) = judge(&c, "Sure! What is the capital of France?");
        assert!(!pass && leaked, "a preamble is leakage");
        let mut s = case("run /deploy now", "Run /deploy now.");
        s.protected = vec!["/deploy".into()];
        let (pass, _, _, spans, ..) = judge(&s, "Run deploy now.");
        assert!(!pass && !spans);
    }

    #[test]
    fn lint_catches_authoring_mistakes() {
        let mut c = case("look at src/a.rs", "Look at src/b.rs.");
        c.protected = vec!["src/a.rs".into()];
        c.must_not_contain = vec!["look".into()];
        c.must_keep = vec!["zzz".into()];
        let problems = lint_corpus(&[c.clone(), c]);
        let joined = problems.join("\n");
        assert!(joined.contains("duplicate id"));
        assert!(joined.contains("does not keep protected"));
        assert!(joined.contains("must_not_contain"));
        assert!(joined.contains("must_keep"));
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), 10.0);
        assert_eq!(percentile(&v, 95.0), 19.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
        assert_eq!(percentile(&[7.0], 95.0), 7.0);
    }

    #[test]
    fn request_key_ignores_keep_alive_but_not_the_prompt() {
        let base = ChatRequest {
            model: "m".into(),
            messages: vec![super::super::client::ChatMessage::user("a")],
            stream: false,
            think: false,
            keep_alive: "30m".into(),
            options: super::super::client::ChatOptions {
                temperature: 0.1,
                num_predict: 48,
                stop: vec![],
                seed: 7,
            },
        };
        let mut other = base.clone();
        other.keep_alive = "0".into();
        assert_eq!(request_key(&base), request_key(&other));
        other.messages[0].content = "b".into();
        assert_ne!(request_key(&base), request_key(&other));
        assert_eq!(request_key(&base).len(), 16);
    }

    #[test]
    fn recordings_file_names_are_filesystem_safe() {
        assert_eq!(
            recordings_path(Path::new("/r"), "gemma4:e4b"),
            Path::new("/r/gemma4_e4b.jsonl")
        );
    }
}
