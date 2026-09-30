//! Deterministic eval tier: replays committed model outputs through the real
//! masking, validators, restore and scoring — no network, GPU or model.
//!
//! What fails this test:
//!
//! - **Prompt drift.** Any change to a prompt, example, sampling option,
//!   mask style or the default model changes request fingerprints; replies
//!   are then missing and the test says so. Re-record with
//!   `just eval-llm gemma4:e4b --record` (and `--corpus
//!   crates/dictate-fmt/tests/eval/mask_stress.jsonl --record`), review the
//!   diff, and commit it with the change.
//! - **A regression** in masking, validation or restore: span preservation
//!   below 100%, any leak, or a pass rate below [`PASS_FLOOR`].
//!
//! # The floor
//!
//! The recorded `gemma4:e4b` run (prompt s21.2, `<k1/>` masks) passes
//! 266/268 = 99.3% of the corpus; the four live runs during tuning ranged
//! 97.4–99.3% as validators were tightened. The floor is 96%: about eight
//! cases of headroom below the recording, so a deliberate, reviewed rubric or
//! validator change can land, while a regression that breaks a category
//! (terminal alone is 149 cases) or a validator that starts rejecting good
//! output wholesale cannot. Replays are deterministic, so the number only
//! moves when code or fixtures change.

use std::path::PathBuf;
use std::sync::Arc;

use dictate_fmt::llm::config::DEFAULT_MODELS;
use dictate_fmt::llm::eval::{self, Case, ReplayBackend};
use dictate_fmt::llm::{LlmConfig, LlmFormatter};

const PASS_FLOOR: f64 = 0.96;

fn eval_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/eval")
}

fn corpus(name: &str) -> Vec<Case> {
    eval::load_corpus(&eval_dir().join(name)).expect("corpus parses")
}

async fn replay(cases: &[Case]) -> (eval::Report, Arc<ReplayBackend>) {
    let model = DEFAULT_MODELS[0];
    let path = eval::recordings_path(&eval_dir().join("recordings"), model);
    let recordings = eval::load_recordings(&path).expect("recordings for the default model");
    let backend = Arc::new(ReplayBackend::new(model, recordings));
    let latency = backend.latency_by_case();
    let formatter = LlmFormatter::with_backend(
        LlmConfig {
            enabled: true,
            models: vec![model.to_string()],
            ..LlmConfig::default()
        },
        backend.clone(),
    );
    let latency_of = |id: &str| latency.get(id).copied().unwrap_or(0.0);
    let results = eval::run_corpus(&formatter, cases, Some(&latency_of), |_, _| {}).await;
    let report = eval::report(model, dictate_fmt::llm::DEFAULT_MASK.as_str(), &results);
    (report, backend)
}

fn assert_no_drift(backend: &ReplayBackend) {
    let misses = backend.misses();
    assert!(
        misses.is_empty(),
        "{} request(s) have no recording — a prompt, option, mask or model changed. \
         Re-record with `just eval-llm gemma4:e4b --record` and review the diff. First: {}",
        misses.len(),
        misses[0]
    );
}

#[test]
fn corpus_is_well_formed_and_weighted_like_real_use() {
    let cases = corpus("corpus.jsonl");
    let problems = eval::lint_corpus(&cases);
    assert!(problems.is_empty(), "corpus problems:\n{}", problems.join("\n"));
    assert!(cases.len() >= 200, "{} cases", cases.len());

    let count = |f: &dyn Fn(&Case) -> bool| cases.iter().filter(|c| f(c)).count();
    let tagged = |t: &'static str| move |c: &Case| c.tags.iter().any(|x| x == t);
    // Coding-agent prompts in a terminal dominate real use.
    let terminal = count(&|c| c.category == dictate_proto::AppCategory::Terminal);
    assert!(terminal * 2 >= cases.len(), "terminal {terminal}/{}", cases.len());
    assert!(count(&tagged("slash")) >= 15);
    assert!(count(&|c| !c.protected.is_empty()) >= 40);
    assert!(count(&tagged("question")) >= 30);
    assert!(count(&tagged("adversarial")) >= 20);
    assert!(count(&tagged("correction")) >= 15);
    // Every category is represented.
    for cat in ["terminal", "editor", "browser", "chat", "email", "document", "other"] {
        assert!(count(&|c| c.category.as_str() == cat) >= 10, "{cat}");
    }
    // The p90 length band (41–70 words) is covered for latency statistics.
    assert!(count(&|c| (26..=70).contains(&c.words())) >= 25);

    let stress = corpus("mask_stress.jsonl");
    assert!(eval::lint_corpus(&stress).is_empty());
    assert!(stress.iter().all(|c| c.protected.len() >= 2));
}

#[tokio::test]
async fn recorded_corpus_meets_the_floor_with_every_span_and_no_leak() {
    let cases = corpus("corpus.jsonl");
    let (report, backend) = replay(&cases).await;
    assert_no_drift(&backend);
    println!("{}", report.summary());

    assert_eq!(
        report.span_preservation, 1.0,
        "protected spans must survive byte for byte: {:?}",
        report.failures
    );
    assert_eq!(report.leaked, 0, "no answer/execution leakage: {:?}", report.failures);
    assert!(
        report.pass_rate >= PASS_FLOOR,
        "pass rate {:.1}% below the {:.0}% floor:\n{}",
        report.pass_rate * 100.0,
        PASS_FLOOR * 100.0,
        report.failures.join("\n")
    );
    // The LLM layer must earn its latency: far above rules-only.
    assert!(report.pass_rate > report.baseline_pass_rate + 0.5);
    // Adversarial dictation is formatted, never obeyed.
    let adversarial = &report.by_tag["adversarial"];
    assert!(
        adversarial.pass_rate >= 0.9,
        "adversarial {}/{}",
        adversarial.pass,
        adversarial.n
    );
}

#[tokio::test]
async fn recorded_mask_stress_keeps_every_span() {
    let cases = corpus("mask_stress.jsonl");
    let (report, backend) = replay(&cases).await;
    assert_no_drift(&backend);
    assert_eq!(report.span_preservation, 1.0, "{:?}", report.failures);
    assert_eq!(report.leaked, 0);
    assert!(report.pass_rate >= PASS_FLOOR, "{:?}", report.failures);
}

#[tokio::test]
async fn replay_detects_prompt_drift() {
    // A request the recordings have never seen must be reported, not
    // silently formatted: this is what makes prompt changes regression-tested.
    let backend = Arc::new(ReplayBackend::new(DEFAULT_MODELS[0], Vec::new()));
    let formatter = LlmFormatter::with_backend(
        LlmConfig {
            enabled: true,
            models: vec![DEFAULT_MODELS[0].to_string()],
            ..LlmConfig::default()
        },
        backend.clone(),
    );
    let outcome = formatter
        .format(&dictate_fmt::llm::LlmRequest::new(
            "this sentence was never recorded anywhere",
        ))
        .await;
    assert!(outcome.error.unwrap().contains("no recording"));
    assert_eq!(backend.misses().len(), 1);
}
