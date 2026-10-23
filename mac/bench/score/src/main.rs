use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::Path,
};

// Same normalization and word edit distance as dictate-devtools `devtool wer`.
// Keep apostrophes inside words; map other punctuation to separators.
fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}
fn distance(r: &[String], h: &[String]) -> usize {
    let mut d: Vec<_> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut next = vec![i + 1];
        for (j, hw) in h.iter().enumerate() {
            next.push(
                (d[j] + usize::from(rw != hw))
                    .min(d[j + 1] + 1)
                    .min(next[j] + 1),
            );
        }
        d = next;
    }
    d[h.len()]
}
fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() as f64 * p).ceil() as usize).saturating_sub(1)]
}
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    let manifest = Path::new(&args[1]);
    let base = manifest.parent().ok_or("manifest needs parent")?;
    let mut references = BTreeMap::new();
    for row in fs::read_to_string(manifest)?.lines() {
        let f: Vec<_> = row.split('\t').collect();
        if f.len() != 4 || references.contains_key(f[0]) {
            return Err("invalid/duplicate manifest row".into());
        }
        references.insert(
            f[0].to_owned(),
            (
                f[1].to_owned(),
                normalize(&fs::read_to_string(base.join(f[3]))?),
            ),
        );
    }
    for file in &args[2..] {
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut load = None;
        for line in fs::read_to_string(file)?.lines() {
            let row: Value = serde_json::from_str(line)?;
            match row["kind"].as_str() {
                Some("load") => {
                    if load.replace(row).is_some() {
                        return Err("duplicate load row".into());
                    }
                }
                Some("clip") => {
                    let id = row["id"].as_str().ok_or("missing id")?;
                    let (set, _) = references.get(id).ok_or("unknown clip")?;
                    let pass = row["pass"].as_u64().ok_or("invalid pass")?;
                    if pass > 2 || !seen.insert((id.to_owned(), pass)) || row["set"] != *set {
                        return Err("invalid/duplicate clip row".into());
                    }
                    if !row["ms"]
                        .as_f64()
                        .is_some_and(|ms| ms.is_finite() && ms > 0.0)
                    {
                        return Err("invalid latency".into());
                    }
                    groups.entry(set.clone()).or_default().push(row);
                }
                _ => return Err("unexpected record".into()),
            }
        }
        if seen.len() != references.len() * 3 {
            return Err(format!("incomplete result: {file}: {} rows", seen.len()).into());
        }
        let load = load.ok_or("missing load")?;
        for (set, rows) in groups {
            let mut times = Vec::new();
            let mut errors = [0; 3];
            let mut words = [0; 3];
            let mut per_clip = BTreeMap::new();
            for row in &rows {
                let id = row["id"].as_str().ok_or("missing id")?;
                let (_, reference) = &references[id];
                let text = row["text"].as_str().ok_or("missing text")?;
                let edits = distance(reference, &normalize(text));
                let pass = row["pass"].as_u64().ok_or("missing pass")? as usize;
                errors[pass] += edits;
                words[pass] += reference.len();
                times.push(row["ms"].as_f64().ok_or("missing ms")?);
                per_clip.entry(id.to_owned()).or_insert(
                    json!({"edits":edits, "words":reference.len(), "audio_s":row["audio_s"]}),
                );
            }
            let p50 = percentile(&mut times, 0.5);
            let p95 = percentile(&mut times, 0.95);
            println!(
                "{}",
                json!({"engine":load["engine"], "set":set, "clips":per_clip.len(), "measurements":rows.len(), "wer_percent":100.0*errors.iter().sum::<usize>() as f64/words.iter().sum::<usize>() as f64, "errors_by_pass":errors, "words_by_pass":words, "p50_ms":p50, "p95_ms":p95, "load_ms":load["ms"], "per_clip":per_clip})
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scores_word_edits_and_preserves_apostrophes_like_devtool() {
        let r = normalize("Hello, WORLD! It's a test.");
        assert_eq!(distance(&r, &normalize("hello world it's a test")), 0);
        assert_eq!(distance(&r, &normalize("hello world a test")), 1);
        assert_eq!(distance(&r, &normalize("hello new world it's the test")), 2);
    }
    #[test]
    fn nearest_rank_percentiles_do_not_hide_tail() {
        let mut v = [4.0, 1.0, 3.0, 2.0];
        assert_eq!(percentile(&mut v, 0.5), 2.0);
        assert_eq!(percentile(&mut v, 0.95), 4.0);
    }
}
