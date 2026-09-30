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
