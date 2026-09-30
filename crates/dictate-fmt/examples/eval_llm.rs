//! Live LLM eval tier (feature `eval-live`). Run through `just eval-llm`.
//!
//! ```text
//! just eval-llm                         # default ladder, corpus, JSON report
//! just eval-llm gemma4:12b              # a specific model
//! just eval-llm gemma4:e4b --record     # re-record the CI fixtures
//! just eval-llm "" --latency 30         # p50/p95 at 17 and 53 words
//! just eval-llm "" --warmup             # cold vs warm vs warm-up-at-record
//! just eval-llm "" --masks              # compare placeholder styles
//! just eval-llm "" --history 300        # real-history aggregates (eval-history)
//! ```
//!
//! Options: `--host URL`, `--corpus PATH`, `--out PATH`, `--mask STYLE`,
//! `--filter SUBSTR`, `--keep-alive DUR`, `--unload`, `--verbose`.
//!
//! Privacy: the corpus is synthetic, so per-case output is written to the
//! report. The `--history` tier reads a local DB read-only and prints and
//! writes aggregates only — never a word of real text.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dictate_fmt::llm::eval::{self, Case, RecordingBackend, Report};
use dictate_fmt::llm::{
    ChatBackend, HttpBackend, LlmConfig, LlmFormatter, LlmRequest, MaskStyle,
};
use dictate_proto::{AppCategory, Tone};

#[derive(Debug, Default)]
struct Args {
    model: Option<String>,
    host: String,
    corpus: PathBuf,
    out: Option<PathBuf>,
    record: bool,
    mask: Option<MaskStyle>,
    masks: bool,
    filter: Option<String>,
    latency: usize,
    warmup: bool,
    history: usize,
    history_db: Option<PathBuf>,
    keep_alive: String,
    unload: bool,
    verbose: bool,
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn parse_args() -> Args {
    let mut a = Args {
        host: "http://localhost:11434".into(),
        corpus: crate_dir().join("tests/eval/corpus.jsonl"),
        keep_alive: "10m".into(),
        ..Args::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| panic!("{arg} needs a value"));
        match arg.as_str() {
            "--model" => {
                let m = val();
                if !m.is_empty() {
                    a.model = Some(m);
                }
            }
            "--host" => a.host = val(),
            "--corpus" => a.corpus = PathBuf::from(val()),
            "--out" => a.out = Some(PathBuf::from(val())),
            "--record" => a.record = true,
            "--mask" => {
                let v = val();
                a.mask = Some(MaskStyle::parse(&v).unwrap_or_else(|| panic!("unknown mask {v}")));
            }
            "--masks" => a.masks = true,
            "--filter" => a.filter = Some(val()),
            "--latency" => a.latency = val().parse().expect("--latency N"),
            "--warmup" => a.warmup = true,
            "--history" => a.history = val().parse().expect("--history N"),
            "--history-db" => a.history_db = Some(PathBuf::from(val())),
            "--keep-alive" => a.keep_alive = val(),
            "--unload" => a.unload = true,
            "--verbose" | "-v" => a.verbose = true,
            "--help" | "-h" => {
                println!("{}", include_str!("eval_llm.rs").lines().take(20).collect::<Vec<_>>().join("\n"));
                std::process::exit(0);
            }
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

fn formatter(args: &Args, model: &str, backend: Arc<dyn ChatBackend>, mask: MaskStyle) -> LlmFormatter {
    let config = LlmConfig {
        enabled: true,
        host: args.host.clone(),
        models: vec![model.to_string()],
        keep_alive: args.keep_alive.clone(),
        ..LlmConfig::default()
    };
    LlmFormatter::with_backend(config, backend).with_mask_style(mask)
}

async fn resolve_model(args: &Args) -> String {
    if let Some(m) = &args.model {
        return m.clone();
    }
    let f = LlmFormatter::new(LlmConfig {
        enabled: true,
        host: args.host.clone(),
        ..LlmConfig::default()
    });
    match f.refresh().await.model() {
        Some(m) => m.to_string(),
        None => panic!("no model on the default ladder is installed: {}", f.health().summary()),
    }
}

async fn run_cases(
    args: &Args,
    model: &str,
    mask: MaskStyle,
    cases: &[Case],
) -> (Report, Vec<eval::CaseResult>, Vec<eval::Recording>) {
    let http: Arc<dyn ChatBackend> = Arc::new(HttpBackend::new(&args.host));
    let recorder = Arc::new(RecordingBackend::new(http));
    let f = formatter(args, model, recorder.clone(), mask);
    // Load before timing anything: cold start is measured by --warmup.
    if let Err(e) = f.warm_up(None).await {
        panic!("cannot load {model}: {e}");
    }
    let mut results = Vec::with_capacity(cases.len());
    let started = Instant::now();
    for (i, case) in cases.iter().enumerate() {
        recorder.set_case(&case.id);
        let mut r = eval::run_corpus(&f, std::slice::from_ref(case), None, |_, _| {}).await;
        let r = r.pop().expect("one result");
        if args.verbose && !r.pass {
            println!(
                "FAIL {} [{}] {:.0}ms {:?}\n  in : {}\n  out: {}\n  exp: {}\n  raw: {:?}",
                r.id,
                r.rejected_by.as_deref().or(r.skipped.as_deref()).unwrap_or("-"),
                r.latency_ms,
                r.fail_reasons,
                case.input,
                r.output,
                case.expected,
                r.raw_outputs
            );
        }
        if (i + 1) % 25 == 0 {
            eprintln!("  {}/{} cases, {:.0}s", i + 1, cases.len(), started.elapsed().as_secs_f64());
        }
        results.push(r);
    }
    let report = eval::report(model, mask.as_str(), &results);
    (report, results, recorder.recordings())
}

fn write_json(path: &Path, value: &impl serde::Serialize) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("create report dir");
    }
    std::fs::write(path, serde_json::to_string_pretty(value).expect("serialize")).expect("write report");
    println!("wrote {}", path.display());
}

fn default_out(model: &str, suffix: &str) -> PathBuf {
    let root = crate_dir().join("../../target/eval");
    eval::recordings_path(&root, &format!("{model}-{suffix}")).with_extension("json")
}

// ---------------------------------------------------------------------------
// Latency and warm-up benchmarks (synthetic text)
// ---------------------------------------------------------------------------

const SENTENCES: &[&str] = &[
    "so I want you to look at the parser module and figure out why it is slow",
    "and then like update the tests so they cover the new error path",
    "can you also check whether the config loader handles missing files",
    "actually no I mean the settings loader not the config one",
    "after that write a short summary of what you changed and why",
    "make sure you don't touch the public API because other crates depend on it",
    "um if anything is unclear just ask me before you start refactoring",
    "I think the tokenizer is fine but double check the the error messages",
    "we should probably also run the benchmarks before and after the change",
    "oh and please keep the commit history clean with one commit per step",
];

/// A synthetic dictation of exactly `words` words, varied by `seed` so the
/// final turn is never a cache hit.
fn synthetic(words: usize, seed: usize) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(words);
    let mut i = seed;
    while out.len() < words {
        out.extend(SENTENCES[i % SENTENCES.len()].split_whitespace());
        i += 3;
    }
    out.truncate(words);
    out.join(" ")
}

async fn latency_bench(args: &Args, model: &str, reps: usize) -> serde_json::Value {
    let f = formatter(args, model, Arc::new(HttpBackend::new(&args.host)), dictate_fmt::llm::DEFAULT_MASK);
    f.warm_up(Some((&AppCategory::Terminal, &Tone::Neutral))).await.expect("load");
    let mut out = serde_json::Map::new();
    for words in [17usize, 53] {
        let mut ms = Vec::new();
        let mut failed = 0;
        let mut tokens = Vec::new();
        for rep in 0..reps {
            let req = LlmRequest {
                category: AppCategory::Terminal,
                ..LlmRequest::new(synthetic(words, rep))
            };
            let (o, trace) = f.format_traced(&req).await;
            if o.error.is_some() {
                failed += 1;
            }
            ms.push(o.duration.as_secs_f64() * 1000.0);
            if let Some(r) = trace.segments.first().and_then(|s| s.response.as_ref()) {
                tokens.push((r.eval_count, r.prompt_eval_count, r.prompt_eval_cached_count, r.eval_ms, r.prompt_eval_ms));
            }
        }
        let stats = eval::LatencyStats::of(&ms);
        let avg = |f: fn(&(u64, u64, u64, f64, f64)) -> f64| {
            tokens.iter().map(f).sum::<f64>() / tokens.len().max(1) as f64
        };
        println!(
            "{words:>3} words: p50 {:.0} ms · p95 {:.0} ms · max {:.0} ms · failed-open {failed}/{reps} · \
             avg out {:.0} tok, prompt {:.0} tok ({:.0} cached), decode {:.0} ms, prompt eval {:.0} ms",
            stats.p50_ms, stats.p95_ms, stats.max_ms,
            avg(|t| t.0 as f64), avg(|t| t.1 as f64), avg(|t| t.2 as f64), avg(|t| t.3), avg(|t| t.4),
        );
        out.insert(
            format!("words_{words}"),
            serde_json::json!({
                "reps": reps, "p50_ms": stats.p50_ms, "p95_ms": stats.p95_ms, "max_ms": stats.max_ms,
                "failed_open": failed,
                "avg_eval_tokens": avg(|t| t.0 as f64), "avg_prompt_tokens": avg(|t| t.1 as f64),
                "avg_cached_prompt_tokens": avg(|t| t.2 as f64),
                "avg_decode_ms": avg(|t| t.3), "avg_prompt_eval_ms": avg(|t| t.4),
            }),
        );
    }
    serde_json::Value::Object(out)
}

async fn wait_unloaded(host: &str, model: &str) {
    let url = format!("{}/api/ps", dictate_fmt::llm::client::normalize_base(host));
    for _ in 0..50 {
        let body = reqwest::get(&url).await.ok();
        let text = match body {
            Some(b) => b.text().await.unwrap_or_default(),
            None => String::new(),
        };
        if !text.contains(&format!("\"name\":\"{model}\"")) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn warmup_bench(args: &Args, model: &str) -> serde_json::Value {
    let http = Arc::new(HttpBackend::new(&args.host));
    let f = Arc::new(formatter(args, model, http.clone(), dictate_fmt::llm::DEFAULT_MASK));
    let req = LlmRequest {
        category: AppCategory::Terminal,
        ..LlmRequest::new(synthetic(17, 1))
    };
    let mut results = serde_json::Map::new();

    // 1. Cold: model unloaded, no warm-up, production timeout.
    f.warm_up(None).await.expect("load");
    f.unload().await.expect("unload");
    wait_unloaded(&args.host, model).await;
    let o = f.format(&req).await;
    println!("cold, production timeout: {:.0} ms, error {:?}", o.duration.as_secs_f64() * 1000.0, o.error);
    results.insert("cold_production_timeout".into(), serde_json::json!({"ms": o.duration.as_secs_f64() * 1000.0, "error": o.error}));

    // 2. Cold with a generous timeout: the raw cost of a cold start.
    f.unload().await.expect("unload");
    wait_unloaded(&args.host, model).await;
    let mut generous = LlmConfig {
        enabled: true,
        host: args.host.clone(),
        models: vec![model.to_string()],
        keep_alive: args.keep_alive.clone(),
        ..LlmConfig::default()
    };
    generous.timeout.max_ms = 30_000;
    generous.timeout.base_ms = 30_000;
    let g = LlmFormatter::with_backend(generous, http.clone());
    let o = g.format(&req).await;
    println!("cold, generous timeout: {:.0} ms, error {:?}", o.duration.as_secs_f64() * 1000.0, o.error);
    results.insert("cold_generous_timeout_ms".into(), serde_json::json!(o.duration.as_secs_f64() * 1000.0));

    // 3. Warm-up at record start, then ~2 s of "speech".
    f.unload().await.expect("unload");
    wait_unloaded(&args.host, model).await;
    f.warm_up_in_background(AppCategory::Terminal, Tone::Neutral);
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let o = f.format(&req).await;
    println!("warm-up at record start + 2 s speech: {:.0} ms, error {:?}", o.duration.as_secs_f64() * 1000.0, o.error);
    results.insert("warmup_on_record_ms".into(), serde_json::json!(o.duration.as_secs_f64() * 1000.0));

    // 4. Warm, prompt cache evicted by another variant vs primed.
    let other = LlmRequest {
        category: AppCategory::Email,
        ..LlmRequest::new(synthetic(17, 5))
    };
    let mut unprimed = Vec::new();
    let mut primed = Vec::new();
    for rep in 0..10 {
        let r = LlmRequest {
            category: AppCategory::Terminal,
            ..LlmRequest::new(synthetic(17, rep + 20))
        };
        f.format(&other).await;
        let (o, t) = f.format_traced(&r).await;
        unprimed.push((o.duration.as_secs_f64() * 1000.0, t.segments[0].response.as_ref().map_or(0.0, |x| x.prompt_eval_ms)));
        f.format(&other).await;
        f.warm_up(Some((&AppCategory::Terminal, &Tone::Neutral))).await.expect("prime");
        let (o, t) = f.format_traced(&r).await;
        primed.push((o.duration.as_secs_f64() * 1000.0, t.segments[0].response.as_ref().map_or(0.0, |x| x.prompt_eval_ms)));
    }
    let med = |v: &[(f64, f64)], i: usize| {
        let mut x: Vec<f64> = v.iter().map(|p| if i == 0 { p.0 } else { p.1 }).collect();
        x.sort_by(f64::total_cmp);
        eval::percentile(&x, 50.0)
    };
    println!(
        "warm, prompt cache evicted: p50 {:.0} ms (prompt eval {:.0} ms) · primed: p50 {:.0} ms (prompt eval {:.0} ms)",
        med(&unprimed, 0), med(&unprimed, 1), med(&primed, 0), med(&primed, 1)
    );
    results.insert("warm_evicted_p50_ms".into(), serde_json::json!(med(&unprimed, 0)));
    results.insert("warm_evicted_prompt_eval_ms".into(), serde_json::json!(med(&unprimed, 1)));
    results.insert("warm_primed_p50_ms".into(), serde_json::json!(med(&primed, 0)));
    results.insert("warm_primed_prompt_eval_ms".into(), serde_json::json!(med(&primed, 1)));
    serde_json::Value::Object(results)
}

// ---------------------------------------------------------------------------
// Real-history tier (aggregates only)
// ---------------------------------------------------------------------------

#[cfg(feature = "eval-history")]
async fn history_tier(args: &Args, model: &str, n: usize) -> serde_json::Value {
    use dictate_fmt::llm::{LlmGate, LlmPlan};
    use std::collections::BTreeMap;

    let path = args.history_db.clone().unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").expect("HOME")).join(".local/share/dictate-agent/history.db")
    });
    // Read-only, and the text never leaves this function except as counts.
    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open history read-only");
    let mut stmt = conn
        .prepare(
            "SELECT raw_transcription FROM interactions \
             WHERE route_type = 'type' AND raw_transcription IS NOT NULL AND length(trim(raw_transcription)) > 0 \
             ORDER BY random() LIMIT ?1",
        )
        .expect("query");
    let texts: Vec<String> = stmt
        .query_map([n as i64], |r| r.get(0))
        .expect("rows")
        .filter_map(Result::ok)
        .collect();
    drop(stmt);
    drop(conn);

    let f = formatter(args, model, Arc::new(HttpBackend::new(&args.host)), dictate_fmt::llm::DEFAULT_MASK);
    f.warm_up(Some((&AppCategory::Terminal, &Tone::Neutral))).await.expect("load");
    let gate = LlmGate::default();
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    let mut rejected: BTreeMap<String, usize> = BTreeMap::new();
    let mut errors: BTreeMap<String, usize> = BTreeMap::new();
    let mut latency: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut ratios = Vec::new();
    let (mut ran, mut applied, mut changed, mut partial, mut slash) = (0, 0, 0, 0, 0);
    for text in &texts {
        if text.trim_start().starts_with('/') {
            slash += 1;
        }
        let req = LlmRequest {
            category: AppCategory::Terminal,
            ..LlmRequest::new(text.clone())
        };
        match f.plan(&req, &gate) {
            LlmPlan::Skip(r) => *skipped.entry(r.to_string()).or_default() += 1,
            LlmPlan::Run => {
                ran += 1;
                let o = f.format(&req).await;
                let words = text.split_whitespace().count();
                latency
                    .entry(eval::bucket(words).to_string())
                    .or_default()
                    .push(o.duration.as_secs_f64() * 1000.0);
                if let Some(r) = &o.validator_rejection {
                    *rejected.entry(r.validator.as_str().to_string()).or_default() += 1;
                }
                match &o.error {
                    Some(e) if !e.starts_with("validator") => {
                        let kind = if e.contains("timed out") { "timeout" } else { "other" };
                        *errors.entry(kind.to_string()).or_default() += 1;
                    }
                    Some(_) => {}
                    None => {
                        applied += 1;
                        if o.segments_applied < o.segments {
                            partial += 1;
                        }
                        ratios.push(o.text.chars().count() as f64 / text.chars().count().max(1) as f64);
                    }
                }
                if o.changed {
                    changed += 1;
                }
            }
        }
    }
    ratios.sort_by(f64::total_cmp);
    let by_bucket: BTreeMap<String, eval::LatencyStats> =
        latency.iter().map(|(k, v)| (k.clone(), eval::LatencyStats::of(v))).collect();
    let all: Vec<f64> = latency.values().flatten().copied().collect();
    let summary = serde_json::json!({
        "sampled": texts.len(),
        "starting_with_slash": slash,
        "skipped": skipped,
        "ran": ran,
        "applied": applied,
        "partially_applied": partial,
        "changed": changed,
        "rejected_by": rejected,
        "errors": errors,
        "latency_all": eval::LatencyStats::of(&all),
        "latency_by_bucket": by_bucket,
        "length_ratio_p5_p50_p95": [eval::percentile(&ratios, 5.0), eval::percentile(&ratios, 50.0), eval::percentile(&ratios, 95.0)],
    });
    println!("history tier (aggregates only):\n{}", serde_json::to_string_pretty(&summary).unwrap());
    summary
}

#[cfg(not(feature = "eval-history"))]
async fn history_tier(_: &Args, _: &str, _: usize) -> serde_json::Value {
    panic!("--history needs `--features eval-history`");
}

// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let args = parse_args();
    let model = resolve_model(&args).await;
    let mask = args.mask.unwrap_or(dictate_fmt::llm::DEFAULT_MASK);
    println!("model {model} at {}", args.host);

    if args.latency > 0 {
        let v = latency_bench(&args, &model, args.latency).await;
        write_json(&args.out.clone().unwrap_or_else(|| default_out(&model, "latency")), &v);
    } else if args.warmup {
        let v = warmup_bench(&args, &model).await;
        write_json(&args.out.clone().unwrap_or_else(|| default_out(&model, "warmup")), &v);
    } else if args.history > 0 {
        let v = history_tier(&args, &model, args.history).await;
        write_json(&args.out.clone().unwrap_or_else(|| default_out(&model, "history")), &v);
    } else {
        let mut cases = eval::load_corpus(&args.corpus).expect("corpus");
        let problems = eval::lint_corpus(&cases);
        assert!(problems.is_empty(), "corpus problems:\n{}", problems.join("\n"));
        if let Some(f) = &args.filter {
            cases.retain(|c| c.id.contains(f.as_str()) || c.tags.iter().any(|t| t == f));
        }
        let styles: Vec<MaskStyle> = if args.masks {
            // Only cases with masked spans say anything about masking.
            cases.retain(|c| !c.protected.is_empty());
            MaskStyle::ALL.to_vec()
        } else {
            vec![mask]
        };
        for style in styles {
            let (report, results, recordings) = run_cases(&args, &model, style, &cases).await;
            println!("{}", report.summary());
            if args.verbose {
                for f in &report.failures {
                    println!("  {f}");
                }
            }
            let out = args
                .out
                .clone()
                .unwrap_or_else(|| default_out(&model, style.as_str()));
            write_json(&out, &serde_json::json!({"report": report, "results": results}));
            if args.record {
                let path = eval::recordings_path(&crate_dir().join("tests/eval/recordings"), &model);
                eval::save_recordings(&path, &recordings).expect("save recordings");
                println!("recorded {} replies to {}", recordings.len(), path.display());
            }
        }
    }

    if args.unload {
        let http = HttpBackend::new(&args.host);
        match http.load(&model, "0").await {
            Ok(_) => println!("unloaded {model}"),
            Err(e) => eprintln!("unload {model}: {e}"),
        }
    }
}
