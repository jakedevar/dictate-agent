//! `cargo run --release -p dictate-dict --example latency` (synthetic only).
use dictate_dict::{Dictionary, DictionaryConfig, DictionaryStore};
use dictate_proto::DictionaryEntry;
use std::{hint::black_box, time::Instant};
fn main() -> anyhow::Result<()> {
    let mut store = DictionaryStore::in_memory()?;
    for i in 0..500 {
        let mut entry = DictionaryEntry::new(format!("SyntheticTerm{i}"));
        entry.sounds_like = vec![format!("synthetic term {i}")];
        store
            .upsert(entry)
            .map_err(|e| anyhow::anyhow!(e.message))?;
    }
    let dictionary = Dictionary::new(store, DictionaryConfig::default(), None)?;
    let text = format!(
        "{} synthetic term 42 and synthetic term 314 for this example.",
        "ordinary words in a sentence ".repeat(18)
    );
    assert_eq!(text.split_whitespace().count(), 100);
    for _ in 0..100 {
        black_box(dictionary.apply(&text, None));
    }
    let mut ns = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        let start = Instant::now();
        let applied = dictionary.apply(black_box(&text), None);
        assert_eq!(applied.replacements.len(), 2);
        black_box(applied);
        ns.push(start.elapsed().as_nanos());
    }
    ns.sort_unstable();
    println!(
        "500 entries, 100 words, 10,000 samples: p50={:.3} ms p95={:.3} ms p99={:.3} ms",
        ns[5000] as f64 / 1e6,
        ns[9500] as f64 / 1e6,
        ns[9900] as f64 / 1e6
    );
    Ok(())
}
