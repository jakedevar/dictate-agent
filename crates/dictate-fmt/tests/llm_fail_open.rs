//! Fail-open and skip-rule behaviour of the LLM formatter, end to end over
//! real HTTP against a fake Ollama on 127.0.0.1 — so the real client, error
//! classification, timeouts and resolver run exactly as in production. No
//! network, GPU or model needed.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dictate_fmt::llm::{
    LlmConfig, LlmFormatter, LlmGate, LlmHealth, LlmPlan, LlmRequest, Validator,
};
use dictate_proto::{AppCategory, Route, SkipReason, Tone};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Fake Ollama
// ---------------------------------------------------------------------------

enum Reply {
    Json(u16, Value),
    Raw(u16, &'static str),
    Hang,
}

type Handler = dyn Fn(&str, &Value) -> Reply + Send + Sync;

struct Fake {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Fake {
    async fn start(handler: impl Fn(&str, &Value) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let log = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let Some((path, body)) = read_request(&mut sock).await else {
                        return;
                    };
                    log.lock().unwrap().push((path.clone(), body.clone()));
                    let (status, payload) = match handler(&path, &body) {
                        Reply::Json(s, v) => (s, v.to_string()),
                        Reply::Raw(s, t) => (s, t.to_string()),
                        Reply::Hang => {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                            return;
                        }
                    };
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        Self { addr, requests }
    }

    fn host(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn chat_requests(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == "/api/chat")
            .map(|(_, b)| b.clone())
            .collect()
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, Value)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let path = head.split_whitespace().nth(1)?.to_string();
    let len = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buf.len() < header_end + len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = serde_json::from_slice(&buf[header_end..]).unwrap_or(Value::Null);
    Some((path, body))
}

fn tags(names: &[&str]) -> Reply {
    Reply::Json(
        200,
        json!({"models": names.iter().map(|n| json!({"name": n, "details": {"family": "x"}})).collect::<Vec<_>>()}),
    )
}

/// The dictation the model was sent (between the delimiters of the last
/// user turn).
fn dictation(body: &Value) -> String {
    let last = body["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default();
    let start = last.find("<dictation>\n").map_or(0, |i| i + "<dictation>\n".len());
    let end = last.rfind("\n</dictation>").unwrap_or(last.len());
    last[start..end].to_string()
}

fn chat(content: &str) -> Reply {
    Reply::Json(
        200,
        json!({"model": "m", "message": {"role": "assistant", "content": content}, "done": true, "done_reason": "stop", "eval_count": 5}),
    )
}

/// A model that capitalizes the first letter and adds a period.
fn tidy(d: &str) -> String {
    let mut c = d.chars();
    let first = c.next().map(|f| f.to_uppercase().collect::<String>()).unwrap_or_default();
    let mut s = format!("{first}{}", c.as_str());
    if !s.ends_with(['.', '?', '!']) {
        s.push('.');
    }
    s
}

fn config(host: &str) -> LlmConfig {
    let mut c = LlmConfig {
        enabled: true,
        host: host.to_string(),
        models: vec!["gemma4:e4b".into(), "gemma4:12b".into()],
        ..LlmConfig::default()
    };
    c.timeout.base_ms = 2000;
    c.timeout.max_ms = 3000;
    c
}

fn request(text: &str) -> LlmRequest {
    LlmRequest {
        category: AppCategory::Terminal,
        ..LlmRequest::new(text)
    }
}

const INPUT: &str = "so could you look at why the build is failing on main";

fn assert_failed_open(outcome: &dictate_fmt::llm::LlmOutcome, input: &str, needle: &str) {
    assert_eq!(outcome.text, input, "fail-open must return the input unchanged");
    assert!(!outcome.changed);
    let err = outcome.error.as_deref().expect("a failure must carry its reason");
    assert!(err.contains(needle), "error {err:?} should mention {needle:?}");
}

// ---------------------------------------------------------------------------
// Fail-open: infrastructure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ollama_down_fails_open_and_then_skips_honestly() {
    // A port nothing listens on.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let f = LlmFormatter::new(config(&format!("http://127.0.0.1:{port}")));
    let started = std::time::Instant::now();
    let out = f.format(&request(INPUT)).await;
    assert_failed_open(&out, INPUT, "unreachable");
    assert!(started.elapsed() < Duration::from_secs(2), "a refused connect is fast");
    assert!(matches!(f.health(), LlmHealth::Unavailable { .. }));
    // Within the back-off the pipeline is told up front: Skipped, not a
    // fabricated Ran, and no per-utterance connection attempt.
    assert_eq!(
        f.plan(&request(INPUT), &LlmGate::default()),
        LlmPlan::Skip(SkipReason::DependencyUnavailable)
    );
}

#[tokio::test]
async fn missing_model_fails_open_then_the_ladder_resolves_to_an_installed_one() {
    let installed = Arc::new(Mutex::new(vec!["gemma4:e4b", "gemma4:12b"]));
    let inst = installed.clone();
    let fake = Fake::start(move |path, body| match path {
        "/api/tags" => tags(&inst.lock().unwrap()),
        _ if body["model"] == "gemma4:e4b" => Reply::Json(
            404,
            json!({"error": "model 'gemma4:e4b' not found"}),
        ),
        _ => chat(&tidy(&dictation(body))),
    })
    .await;
    let f = LlmFormatter::new(config(&fake.host()));

    let out = f.format(&request(INPUT)).await;
    assert_failed_open(&out, INPUT, "not found");
    // The model was removed from Ollama: next use re-probes.
    installed.lock().unwrap().retain(|m| *m != "gemma4:e4b");
    assert_eq!(f.plan(&request(INPUT), &LlmGate::default()), LlmPlan::Run);

    let out = f.format(&request(INPUT)).await;
    assert_eq!(out.error, None);
    assert_eq!(out.model.as_deref(), Some("gemma4:12b"));
    assert_eq!(out.text, tidy(INPUT));
    assert_eq!(
        f.health(),
        LlmHealth::Ready {
            model: "gemma4:12b".into(),
            missing_preferred: vec!["gemma4:e4b".into()]
        }
    );
}

#[tokio::test]
async fn nothing_on_the_ladder_installed_names_the_alternatives() {
    // Today's failure: the configured `qwen3:14b` is gone.
    let fake = Fake::start(|path, _| match path {
        "/api/tags" => tags(&["qwen3.6:27b", "qwen3-embedding:0.6b"]),
        _ => panic!("no chat call may be made without a resolved model"),
    })
    .await;
    let mut c = config(&fake.host());
    c.models = vec!["qwen3:14b".into()];
    let f = LlmFormatter::new(c);
    let health = f.refresh().await;
    assert_eq!(
        health,
        LlmHealth::Unavailable {
            reason: "none of the configured models is installed: qwen3:14b".into(),
            installed_alternatives: vec!["qwen3.6:27b".into()],
        }
    );
    let out = f.format(&request(INPUT)).await;
    assert_failed_open(&out, INPUT, "qwen3.6:27b");
    assert!(fake.chat_requests().is_empty());
}

#[tokio::test]
async fn timeout_fails_open_within_the_scaled_budget() {
    let fake = Fake::start(|path, _| match path {
        "/api/tags" => tags(&["gemma4:e4b"]),
        _ => Reply::Hang,
    })
    .await;
    let mut c = config(&fake.host());
    c.timeout.base_ms = 100;
    c.timeout.per_word_ms = 5;
    c.timeout.max_ms = 400;
    let f = LlmFormatter::new(c);
    let started = std::time::Instant::now();
    let out = f.format(&request(INPUT)).await;
    let elapsed = started.elapsed();
    assert_failed_open(&out, INPUT, "timed out");
    // 100 + 5 * 12 words = 160 ms, plus scheduling; far below the hang.
    assert!(elapsed < Duration::from_millis(1000), "{elapsed:?}");
    assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
    // One slow utterance is not a health change.
    assert!(matches!(f.health(), LlmHealth::Ready { .. }));
}

#[tokio::test]
async fn malformed_reply_fails_open() {
    let fake = Fake::start(|path, _| match path {
        "/api/tags" => tags(&["gemma4:e4b"]),
        _ => Reply::Raw(200, "this is not json"),
    })
    .await;
    let f = LlmFormatter::new(config(&fake.host()));
    let out = f.format(&request(INPUT)).await;
    assert_failed_open(&out, INPUT, "malformed");
}

#[tokio::test]
async fn server_error_fails_open() {
    let fake = Fake::start(|path, _| match path {
        "/api/tags" => tags(&["gemma4:e4b"]),
        _ => Reply::Json(500, json!({"error": "CUDA out of memory"})),
    })
    .await;
    let f = LlmFormatter::new(config(&fake.host()));
    let out = f.format(&request(INPUT)).await;
    assert_failed_open(&out, INPUT, "CUDA out of memory");
}

// ---------------------------------------------------------------------------
// Fail-open: every validator
// ---------------------------------------------------------------------------

async fn rejected_by(
    input: &str,
    category: AppCategory,
    reply: impl Fn(&str) -> Reply + Send + Sync + 'static,
    loosen_edit_distance: bool,
) -> Validator {
    let fake = Fake::start(move |path, body| match path {
        "/api/tags" => tags(&["gemma4:e4b"]),
        _ => reply(&dictation(body)),
    })
    .await;
    let mut f = LlmFormatter::new(config(&fake.host()));
    if loosen_edit_distance {
        f = f.with_thresholds(dictate_fmt::llm::Thresholds {
            max_edit_ratio: 100.0,
            ..Default::default()
        });
    }
    let req = LlmRequest {
        category,
        ..LlmRequest::new(input)
    };
    let out = f.format(&req).await;
    let rejection = out
        .validator_rejection
        .clone()
        .expect("expected a validator rejection");
    assert_failed_open(&out, input, rejection.validator.as_str());
    assert!(out.error.as_deref().unwrap().starts_with("validator rejected output"));
    rejection.validator
}

#[tokio::test]
async fn each_validator_rejects_and_fails_open() {
    use AppCategory::{Chat, Document, Terminal};
    let long = "okay so I would like you to refactor the parser module and also update the tests \
                and then make sure the documentation reflects the new behavior";
    let cases: Vec<(&str, AppCategory, Box<dyn Fn(&str) -> Reply + Send + Sync>, bool, Validator)> = vec![
        (INPUT, Terminal, Box::new(|_| chat("")), false, Validator::EmptyOutput),
        (
            INPUT,
            Terminal,
            Box::new(|d| {
                Reply::Json(200, json!({"message": {"content": tidy(d)}, "done_reason": "length"}))
            }),
            false,
            Validator::Truncated,
        ),
        (
            "please check src/auth.rs and fix it",
            Terminal,
            Box::new(|_| chat("Please check the auth file and fix it.")),
            false,
            Validator::ProtectedSpans,
        ),
        (INPUT, Terminal, Box::new(|d| chat(&format!("Formatted text: {}", tidy(d)))), false, Validator::PromptEcho),
        (INPUT, Chat, Box::new(|d| chat(&format!("Sure! {}", tidy(d)))), false, Validator::Preamble),
        (INPUT, Document, Box::new(|d| chat(&format!("**{}**", tidy(d)))), false, Validator::Markup),
        (
            "first check the logs then restart the service",
            Terminal,
            Box::new(|_| chat("First, check the logs.\nThen restart the service.")),
            false,
            Validator::NewLines,
        ),
        (
            "is the build green on main?",
            Chat,
            Box::new(|_| chat("The build is green on main.")),
            false,
            Validator::Question,
        ),
        (
            "/create_plan uh actually no /research_codebase first, look at the config",
            Terminal,
            Box::new(|_| chat("<k1/> <k2/> first, look at the config.")),
            false,
            Validator::SpanCorrection,
        ),
        ("meet me at five", Chat, Box::new(|_| chat("Meet me at 6.")), false, Validator::Numbers),
        (
            "add fuzzy finding to the file picker",
            Terminal,
            Box::new(|_| chat("Add a fuzzy finding to the file picker.")),
            false,
            Validator::NovelWords,
        ),
        (
            "first run the tests then deploy to staging and then tell the team",
            Terminal,
            Box::new(|_| chat("Tell the team, deploy to staging, run the tests.")),
            false,
            Validator::EditDistance,
        ),
        (long, Terminal, Box::new(|_| chat("Refactor the parser.")), false, Validator::LengthRatio),
    ];
    for (input, category, reply, loosen, want) in cases {
        let got = rejected_by(input, category, reply, loosen).await;
        assert_eq!(got, want, "input {input:?}");
    }
}

// ---------------------------------------------------------------------------
// Protected spans never reach the model in a rewritable form
// ---------------------------------------------------------------------------

#[tokio::test]
async fn leading_slash_command_is_never_sent_and_paths_are_masked() {
    let fake = Fake::start(|path, body| match path {
        "/api/tags" => tags(&["gemma4:e4b"]),
        _ => chat(&tidy(&dictation(body))),
    })
    .await;
    let f = LlmFormatter::new(config(&fake.host()));
    // No caller spans at all: the fallback detector protects them.
    let input = "/research_codebase i would like you to look at src/auth/session.rs and parse_token";
    let out = f.format(&request(input)).await;
    assert_eq!(out.error, None);
    assert_eq!(
        out.text,
        "/research_codebase I would like you to look at src/auth/session.rs and parse_token."
    );
    let sent = dictation(&fake.chat_requests()[0]);
    assert_eq!(sent, "i would like you to look at <k1/> and <k2/>");
    for leaked in ["research", "src/", "auth", "session", "parse_token"] {
        assert!(!sent.contains(leaked), "model saw {leaked:?}: {sent:?}");
    }
}

#[tokio::test]
async fn caller_spans_are_honoured_and_invalid_ones_fail_open() {
    let fake = Fake::start(|path, body| match path {
        "/api/tags" => tags(&["gemma4:e4b"]),
        _ => chat(&tidy(&dictation(body))),
    })
    .await;
    let mut c = config(&fake.host());
    c.protect_fallback = false;
    let f = LlmFormatter::new(c);
    let text = "tell kubernetes team the deploy failed";
    let span = text.find("kubernetes").unwrap();
    let req = LlmRequest {
        protected: vec![span..span + "kubernetes".len()],
        ..request(text)
    };
    let out = f.format(&req).await;
    assert_eq!(out.text, "Tell kubernetes team the deploy failed.");
    assert!(dictation(&fake.chat_requests()[0]).contains("<k1/>"));

    let bad = LlmRequest {
        protected: vec![0..999],
        ..request(text)
    };
    let out = f.format(&bad).await;
    assert_failed_open(&out, text, "invalid protected range");
}

// ---------------------------------------------------------------------------
// Request shape, chunking, warm-up
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_is_bounded_deterministic_and_non_thinking() {
    let fake = Fake::start(|path, body| match path {
        "/api/tags" => tags(&["gemma4:e4b"]),
        _ => chat(&tidy(&dictation(body))),
    })
    .await;
    let mut c = config(&fake.host());
    c.keep_alive = "45m".into();
    let f = LlmFormatter::new(c);
    f.format(&request(INPUT)).await;
    let body = &fake.chat_requests()[0];
    assert_eq!(body["think"], false);
    assert_eq!(body["stream"], false);
    assert_eq!(body["keep_alive"], "45m");
    assert!(body["options"]["temperature"].as_f64().unwrap() <= 0.2);
    let np = body["options"]["num_predict"].as_i64().unwrap();
    assert!((48..=128).contains(&np), "num_predict {np} bounded by input length");
    assert!(body["options"]["stop"]
        .as_array()
        .unwrap()
        .contains(&json!("</dictation>")));
    assert_eq!(body["model"], "gemma4:e4b");
}

#[tokio::test]
async fn long_input_is_chunked_and_a_rejected_chunk_falls_back_alone() {
    let fake = Fake::start(|path, body| match path {
        "/api/tags" => tags(&["gemma4:e4b"]),
        _ => {
            let d = dictation(body);
            if d.contains("second") {
                chat(&format!("Sure! {}", tidy(&d)))
            } else {
                chat(&tidy(&d))
            }
        }
    })
    .await;
    let mut c = config(&fake.host());
    c.chunking.max_single_words = 8;
    c.chunking.chunk_words = 6;
    let f = LlmFormatter::new(c);
    let input = "this is the first sentence here. this is the second sentence here. this is the third one";
    let out = f.format(&request(input)).await;
    assert_eq!(out.segments, 3);
    assert_eq!(out.segments_applied, 2);
    assert_eq!(out.error, None, "partial application is not a failure");
    assert_eq!(
        out.validator_rejection.as_ref().map(|r| r.validator),
        Some(Validator::Preamble)
    );
    assert_eq!(
        out.text,
        "This is the first sentence here. this is the second sentence here. This is the third one."
    );
    assert_eq!(fake.chat_requests().len(), 3);
}

#[tokio::test]
async fn warm_up_loads_and_primes_the_prompt_cache() {
    let fake = Fake::start(|path, _| match path {
        "/api/tags" => tags(&["gemma4:e4b"]),
        _ => Reply::Json(200, json!({"message": {"content": ""}, "done_reason": "load"})),
    })
    .await;
    let f = LlmFormatter::new(config(&fake.host()));
    f.warm_up(Some((&AppCategory::Terminal, &Tone::Neutral)))
        .await
        .unwrap();
    let chats = fake.chat_requests();
    assert_eq!(chats.len(), 2);
    assert_eq!(chats[0]["messages"], json!([]), "first a load-only request");
    assert_eq!(chats[1]["options"]["num_predict"], 1, "then a one-token prime");

    // The background variant never blocks the caller and dedupes.
    let f = Arc::new(f);
    f.warm_up_in_background(AppCategory::Terminal, Tone::Neutral);
    f.warm_up_in_background(AppCategory::Terminal, Tone::Neutral);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.chat_requests().len(), 4, "one warm-up in flight at a time");
}

// ---------------------------------------------------------------------------
// Skip rules
// ---------------------------------------------------------------------------

#[test]
fn skip_rules_are_decided_before_the_model_is_called() {
    let mut c = config("http://127.0.0.1:9");
    c.categories.chat.enabled = false;
    let f = LlmFormatter::new(c.clone());
    let run = |f: &LlmFormatter, req: &LlmRequest, gate: LlmGate| f.plan(req, &gate);
    let r = request(INPUT);
    let type_route = LlmGate::default();

    assert_eq!(run(&f, &r, type_route.clone()), LlmPlan::Run, "unchecked health still runs");
    for route in [Route::Local, Route::Timer, Route::Command, Route::Edit] {
        assert_eq!(
            run(&f, &r, LlmGate { route, ..LlmGate::default() }),
            LlmPlan::Skip(SkipReason::RouteNotEligible)
        );
    }
    let off = LlmGate {
        session_format_llm: Some(false),
        ..LlmGate::default()
    };
    assert_eq!(run(&f, &r, off), LlmPlan::Skip(SkipReason::Disabled));
    let profile_off = LlmGate {
        profile_llm_format: Some(false),
        ..LlmGate::default()
    };
    assert_eq!(run(&f, &r, profile_off.clone()), LlmPlan::Skip(SkipReason::Disabled));
    let forced = LlmGate {
        profile_llm_format: Some(false),
        session_format_llm: Some(true),
        ..LlmGate::default()
    };
    assert_eq!(run(&f, &r, forced.clone()), LlmPlan::Run, "the session overrides the profile");

    let chat = LlmRequest {
        category: AppCategory::Chat,
        ..request(INPUT)
    };
    assert_eq!(run(&f, &chat, type_route.clone()), LlmPlan::Skip(SkipReason::Disabled));

    let short = request("fix it");
    assert_eq!(run(&f, &short, type_route.clone()), LlmPlan::Skip(SkipReason::BelowMinWords));
    assert_eq!(run(&f, &short, forced), LlmPlan::Run, "Some(true) overrides min_words");
    // Protected words do not count: nothing left to format.
    let command_only = request("/compact src/main.rs");
    assert_eq!(
        run(&f, &command_only, type_route.clone()),
        LlmPlan::Skip(SkipReason::BelowMinWords)
    );
    let long = request(&"word ".repeat(c.chunking.max_words + 1));
    assert_eq!(run(&f, &long, type_route.clone()), LlmPlan::Skip(SkipReason::TooLong));

    let disabled = LlmFormatter::new(LlmConfig {
        enabled: false,
        ..c
    });
    assert_eq!(run(&disabled, &r, type_route), LlmPlan::Skip(SkipReason::Disabled));
    assert_eq!(disabled.health(), LlmHealth::Disabled);
}
