//! Synthetic dictation for performance checks (shared by the perf guard test
//! and `examples/chain_bench.rs`). Invented text that exercises every rule:
//! fillers, stutters, numbers, times, paths, slash commands, identifiers.

/// One ~100-word block of the kind of prompt Jake dictates.
const BLOCK: &str = "um so the the retry logic in src/uploader.rs is \
wrong because it waits thirty seconds between attempts and gives up after \
five tries, uh, which is too aggressive for the batch job. can you research \
code base for other callers of retry_with_backoff and check whether \
~/.cloud/settings.json overrides the timeout? i think we should use two point \
five seconds with jitter and cap it at twenty five percent of the budget. \
then create plan, and ping @oncall in #infra at five thirty pm if the p99 \
goes above one hundred and twenty milliseconds. wh- what about the \
getUserName path at https://api.example.com/v1/users, does it retry too";

/// Roughly `words` words of synthetic dictation.
#[must_use]
pub fn dictation(words: usize) -> String {
    let per_block = BLOCK.split_whitespace().count();
    let mut out = String::with_capacity(words * 7);
    let mut have = 0;
    while have < words {
        if !out.is_empty() {
            out.push_str(". ");
        }
        let take = (words - have).min(per_block);
        let block: Vec<&str> = BLOCK.split_whitespace().take(take).collect();
        out.push_str(&block.join(" "));
        have += take;
    }
    out
}
