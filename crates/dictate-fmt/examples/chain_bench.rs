//! Latency of the default text chain on synthetic dictation.
//!
//! ```bash
//! cargo run --release -p dictate-fmt --example chain_bench
//! ```
//!
//! Budget (Wave 2 contract §4): p50 < 1 ms per 100 words, < 5 ms for 1,500.

#[path = "../tests/support/mod.rs"]
mod support;

use std::time::Instant;

use dictate_fmt::{FormatContext, TextChain};

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

fn main() {
    let chain = TextChain::default();
    let ctx = FormatContext::default();
    for (words, iters) in [(17usize, 5_000usize), (100, 5_000), (1_500, 1_000)] {
        let input = support::dictation(words);
        // Warm up allocator and caches.
        for _ in 0..50 {
            std::hint::black_box(chain.format(&input, &ctx));
        }
        let mut samples: Vec<f64> = (0..iters)
            .map(|_| {
                let started = Instant::now();
                std::hint::black_box(chain.format(std::hint::black_box(&input), &ctx));
                started.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        samples.sort_by(f64::total_cmp);
        let run = chain.run(&input, &ctx);
        println!(
            "{words:>5} words: p50 {:>8.1} µs  p90 {:>8.1} µs  p99 {:>8.1} µs  max {:>8.1} µs",
            percentile(&samples, 0.50),
            percentile(&samples, 0.90),
            percentile(&samples, 0.99),
            samples[samples.len() - 1],
        );
        println!("       stages (one run): {}", run.timings);
    }
}
