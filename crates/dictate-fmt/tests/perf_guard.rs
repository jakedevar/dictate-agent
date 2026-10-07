//! A coarse regression guard on the chain's complexity.
//!
//! This is not the latency measurement — that is `examples/chain_bench.rs`,
//! run in release mode (numbers in the S20 handoff). This test only has to
//! catch an accidental quadratic stage, and it must never flake on a loaded
//! CI box or in a debug build. So it formats 10× the longest dictation ever
//! seen (15,000 words vs ~1,500), where linear work takes a few tens of
//! milliseconds even unoptimized and quadratic work takes many seconds, and
//! allows two full seconds for the best of three runs.

mod support;

use std::time::{Duration, Instant};

use dictate_fmt::{FormatContext, TextChain};

#[test]
fn the_chain_stays_linear_on_ten_times_the_longest_dictation() {
    let input = support::dictation(15_000);
    let chain = TextChain::default();
    let ctx = FormatContext::default();
    let best = (0..3)
        .map(|_| {
            let started = Instant::now();
            let out = chain.format(&input, &ctx);
            let took = started.elapsed();
            assert!(out.len() > input.len() / 2, "the chain produced text");
            took
        })
        .min()
        .expect("three runs");
    assert!(
        best < Duration::from_secs(2),
        "15,000 words took {best:?}; a stage has probably gone quadratic"
    );
}

#[test]
fn the_synthetic_input_is_what_the_guard_claims() {
    let text = support::dictation(1_500);
    let words = text.split_whitespace().count();
    assert!((1_450..=1_550).contains(&words), "{words} words");
}

#[test]
fn pathological_runs_of_deletions_stay_linear() {
    let chain = TextChain::default();
    let ctx = FormatContext::default();
    for word in ["um", "the", "uh,"] {
        let input = vec![word; 15_000].join(" ");
        let started = Instant::now();
        let out = chain.format(&input, &ctx);
        let took = started.elapsed();
        assert!(out.len() < input.len(), "{word}: something was collapsed");
        assert!(
            took < Duration::from_secs(2),
            "15,000 × {word:?} took {took:?}; deletion scans have gone quadratic"
        );
    }
}

#[test]
fn pathological_detector_inputs_stay_linear() {
    let chain = TextChain::default();
    let ctx = FormatContext::default();
    let many_ticks = vec!["`a` word"; 7_500].join(" ");
    let one_huge_chunk = format!("see foo({}", ")".repeat(60_000));
    for input in [many_ticks, one_huge_chunk] {
        let started = Instant::now();
        let _ = chain.format(&input, &ctx);
        let took = started.elapsed();
        assert!(
            took < Duration::from_secs(2),
            "{} bytes took {took:?}; span detection has gone quadratic",
            input.len()
        );
    }
}

/// `NONLINEAR_PROTECTION_PATHS`: verifying formatter output against many
/// distinct protected spans is one indexed pass, not a scan per span.
#[test]
fn verifying_many_distinct_spans_stays_linear() {
    use dictate_fmt::text::TextDoc;
    let input: String = (0..20_000)
        .map(|i| format!("run /cmd_{i} now"))
        .collect::<Vec<_>>()
        .join(" ");
    let doc = TextDoc::protected(&input);
    assert_eq!(doc.spans().len(), 20_000);
    let output = doc.restore();
    let started = Instant::now();
    assert_eq!(doc.verify_output(&output), Ok(()));
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "verifying 20,000 distinct spans took {took:?}; verification has gone quadratic"
    );
}

/// `NONLINEAR_PROTECTION_PATHS`: unmatched backtick runs of many distinct
/// lengths, followed by many single runs, must not make each unmatched run
/// rescan the rest of the text for a closer.
#[test]
fn unmatched_backtick_runs_stay_linear() {
    let n = 1_000;
    let mut input = String::new();
    for len in 2..n + 2 {
        input.push_str(&"`".repeat(len));
        input.push_str(" x ");
    }
    for _ in 0..n * n / 2 {
        input.push_str("`a ");
    }
    let started = Instant::now();
    let spans = dictate_fmt::text::detect_spans(&input);
    let took = started.elapsed();
    assert!(!spans.is_empty(), "the single runs pair up");
    assert!(
        took < Duration::from_secs(2),
        "{} bytes of backtick runs took {took:?}; pairing has gone superlinear",
        input.len()
    );
}
