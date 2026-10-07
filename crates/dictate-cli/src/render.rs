//! Turning protocol values into something a person reads in a terminal.
//!
//! Two rules the protocol makes explicit, honored here rather than papered
//! over:
//!
//! - **`awaiting_consent` is not a result.** It is a pending portal prompt, so
//!   it is rendered as pending and never as success or failure.
//! - **An absent transcript is not an empty one.** History shows `—` for text
//!   that is being withheld and `(silence)` for a session where nothing was
//!   said, because those are different facts.

use dictate_proto::{
    CommandResult, Event, HistoryAnalytics, HistoryPage, InjectionOutcome, StageTiming,
    StageTimings, Status, Transcript,
};

/// Print a command result.
pub fn result(result: &CommandResult, json: bool) {
    if json {
        println!("{}", serde_json::to_string(result).unwrap_or_default());
        return;
    }
    match result {
        CommandResult::Ack => println!("ok"),
        CommandResult::SessionStarted { session_id } => {
            println!("recording  (session {})", session_id.as_str());
        }
        CommandResult::SessionStopped { session_id } => {
            println!("stopping   (session {})", session_id.as_str());
        }
        CommandResult::SessionCancelled { session_id } => {
            println!("cancelled  (session {})", session_id.as_str());
        }
        CommandResult::Status(status) => print_status(status),
        CommandResult::Context(profile) => {
            // JSON includes all overrides, making discovery useful for writing
            // profiles without linking the context/X11 implementation.
            println!(
                "{}",
                serde_json::to_string_pretty(profile).expect("profile is serializable")
            );
        }
        CommandResult::History(page) => print_history(page),
        CommandResult::HistoryAnalytics(analytics) => print_history_analytics(analytics),
        CommandResult::Transcript(t) => print_transcript(t),
        CommandResult::Handshake(h) => {
            println!("{} {}", h.server.name, h.server.version);
        }
        CommandResult::Dictionary { entries } => {
            for e in entries {
                println!(
                    "{:<5} {:<24} {:<8} {}{}",
                    e.id.map(|id| id.to_string()).unwrap_or_default(),
                    e.phrase,
                    if e.enabled { "enabled" } else { "disabled" },
                    e.sounds_like.join(", "),
                    if e.apps.is_empty() {
                        String::new()
                    } else {
                        format!(" [apps: {}]", e.apps.join(", "))
                    }
                );
            }
        }
        CommandResult::DictionaryEntry { entry } => println!(
            "{} {}",
            entry.id.map(|id| id.to_string()).unwrap_or_default(),
            entry.phrase
        ),
        CommandResult::DictionarySuggestions { suggestions } => {
            for s in suggestions {
                println!(
                    "{:<24} {} occurrences / {} days ({}) {} → {}",
                    s.entry.phrase,
                    s.count,
                    s.days,
                    s.reason,
                    s.entry.sounds_like.join(", "),
                    s.entry.phrase
                );
            }
        }
        CommandResult::Deleted { id } => println!("deleted {id}"),
        CommandResult::Snippets { snippets } => {
            for s in snippets {
                // One line each: a multi-line expansion shows its first line.
                let first = s.expansion.lines().next().unwrap_or("");
                let more = if s.expansion.lines().nth(1).is_some() {
                    " …"
                } else {
                    ""
                };
                println!(
                    "{:<5} {:<24} {:<8} {first}{more}{}",
                    s.id.map(|id| id.to_string()).unwrap_or_default(),
                    s.trigger,
                    if s.enabled { "enabled" } else { "disabled" },
                    if s.apps.is_empty() {
                        String::new()
                    } else {
                        format!(" [apps: {}]", s.apps.join(", "))
                    }
                );
            }
        }
        CommandResult::Snippet { snippet } => println!(
            "{} {}",
            snippet.id.map(|id| id.to_string()).unwrap_or_default(),
            snippet.trigger
        ),
        // A result this build does not model. Degrade rather than fail — the
        // daemon may simply be newer than the CLI.
        other => println!("{}", other.name()),
    }
}

fn print_history_analytics(analytics: &HistoryAnalytics) {
    match analytics.overall_wpm {
        Some(wpm) => println!("wpm      {wpm:.1}"),
        None => println!("wpm      —"),
    }
    println!("today    {} words", analytics.words_today);
    println!(
        "streak   {} days (best {})",
        analytics.current_streak_days, analytics.longest_streak_days
    );
    for day in &analytics.words_by_day {
        println!("{:<10} {}", day.day, day.words);
    }
}

/// The formatter's observed health. A formatter that fails open is invisible
/// everywhere else, so anything but healthy is spelled out and points at
/// `dictate doctor`.
fn formatter_line(f: &dictate_proto::FormatterStatus) -> String {
    use dictate_proto::FormatterHealth as H;
    let model = f.model.as_deref().unwrap_or("?");
    match &f.health {
        H::Disabled => "format   disabled".to_string(),
        H::Ok => format!("format   {model} (ok)"),
        H::Unchecked => format!("format   {model} (not checked yet)"),
        other => format!(
            "format   {model} — {}: dictations are typed UNFORMATTED{} (run `dictate doctor`)",
            other.as_str().replace('_', " ").to_uppercase(),
            f.detail
                .as_deref()
                .map(|d| format!(" [{d}]"))
                .unwrap_or_default()
        ),
    }
}

/// Whether the microphone is open — the privacy-relevant fact.
fn audio_line(a: &dictate_proto::AudioStatus) -> String {
    if !a.capture_enabled {
        return "mic      disabled (audio-less mode: uploads only)".to_string();
    }
    match (a.input_open, a.pre_roll_ms.unwrap_or(0)) {
        (true, ms) if ms > 0 => {
            format!("mic      OPEN while idle for the {ms} ms pre-roll (audio.pre_roll_ms = 0 closes it)")
        }
        (true, _) => "mic      open (recording)".to_string(),
        (false, 0) => "mic      closed while idle (pre-roll off)".to_string(),
        (false, ms) => format!("mic      not open (wanted for the {ms} ms pre-roll)"),
    }
}

fn print_status(status: &Status) {
    println!("state    {}", status.state.as_str());
    if let Some(session) = &status.session {
        println!(
            "session  {} ({}, {})",
            session.session_id.as_str(),
            session.state.as_str(),
            session.mode.as_str()
        );
    }
    println!(
        "daemon   {} {} (protocol v{})",
        status.daemon.name, status.daemon.version, status.daemon.protocol_version
    );
    if let Some(pid) = status.daemon.pid {
        print!("pid      {pid}");
        if let Some(uptime) = status.daemon.uptime_ms {
            print!("  up {}", human_duration(uptime));
        }
        println!();
    }
    if let Some(model) = &status.model {
        println!(
            "model    {} ({}{})",
            model.name,
            if model.loaded { "loaded" } else { "not loaded" },
            model
                .backend
                .as_ref()
                .map(|b| format!(", {b}"))
                .unwrap_or_default()
        );
    }

    if let Some(formatter) = &status.formatter {
        println!("{}", formatter_line(formatter));
    }
    if let Some(audio) = &status.audio {
        println!("{}", audio_line(audio));
    }

    let f = &status.capabilities.features;
    let mut on: Vec<&str> = Vec::new();
    for (name, enabled) in [
        ("inject", f.text_injection),
        ("capture", f.host_capture),
        ("history", f.history_read),
        ("dictionary", f.dictionary_read),
        ("snippets", f.snippets_read),
        ("config", f.config_read),
    ] {
        if enabled {
            on.push(name);
        }
    }
    println!(
        "allows   {}",
        if on.is_empty() {
            "—".into()
        } else {
            on.join(" ")
        }
    );
    if f.headless {
        println!("         headless — text injection is not possible here");
    }
}

/// The one-line route/injection/timing summary that follows a transcript.
pub fn transcript_summary(t: &Transcript) -> String {
    format!(
        "route {}  {}  {}",
        t.route.as_str(),
        injection(&t.injection),
        timings(&t.timings)
    )
}

fn print_transcript(t: &Transcript) {
    println!("{}", t.text.as_str());
    println!(
        "  route {}  {}  {}",
        t.route.as_str(),
        injection(&t.injection),
        timings(&t.timings)
    );
}

fn print_history(page: &HistoryPage) {
    if page.items.is_empty() {
        println!("no dictations recorded");
        return;
    }
    for item in &page.items {
        let text = match item.text.as_deref() {
            // Absent and empty are different facts and must not be collapsed.
            None => "—".to_string(),
            Some("") => "(silence)".to_string(),
            Some(t) => truncate(t, 68),
        };
        println!("{:>6}  {:<8} {}", item.id, item.route.as_str(), text);
        if let Some(err) = &item.error {
            println!("        ! {}", err.message);
        }
    }
    if let Some(total) = page.total {
        print!("\n{} of {total}", page.items.len());
        if let Some(next) = page.next_offset {
            print!("  (more: --limit N with offset {next})");
        }
        println!();
    }
}

/// Print an event, one line each.
pub fn event(event: &Event, json: bool) {
    if json {
        println!("{}", serde_json::to_string(event).unwrap_or_default());
        return;
    }
    match event {
        Event::StateChanged { from, to, .. } => {
            println!("{:<12} {} -> {}", "state", from.as_str(), to.as_str());
        }
        Event::Final { transcript, .. } => {
            println!("{:<12} {}", "final", transcript.text.as_str());
            println!(
                "             {}  {}",
                injection(&transcript.injection),
                timings(&transcript.timings)
            );
        }
        Event::Partial {
            hypothesis, seq, ..
        } => {
            // Never injectable, and labelled so nobody is tempted.
            println!("{:<12} [{seq}] {}", "partial?", hypothesis.display_text());
        }
        Event::InjectionResolved { outcome, .. } => {
            println!("{:<12} {}", "injection", injection(outcome));
        }
        Event::Error { error, .. } => {
            println!(
                "{:<12} {} ({})",
                "error",
                error.message,
                error.code.as_str()
            );
        }
        Event::AudioLevel { rms, .. } => {
            println!("{:<12} {rms:.2}", "level");
        }
        Event::AudioActivity { activity, .. } => {
            println!("{:<12} {activity:?}", "audio");
        }
        Event::Unknown => println!("{:<12} (from a newer daemon)", "unknown"),
        Event::ContextResolved { context, .. } => {
            println!(
                "{:<12} {}",
                "context",
                context.as_ref().map_or("none", |c| c.app.as_str())
            );
        }
    }
}

fn injection(outcome: &InjectionOutcome) -> String {
    match outcome {
        InjectionOutcome::Injected { method, chars } => {
            format!("injected {chars} chars via {}", method.as_str())
        }
        // Explicitly *not* reported as success or failure: the portal prompt
        // is still open and `injection_resolved` will settle it.
        InjectionOutcome::AwaitingConsent { backend, .. } => {
            format!("awaiting consent from {backend}")
        }
        InjectionOutcome::ConsentDenied { backend, .. } => format!("consent denied by {backend}"),
        InjectionOutcome::Unavailable { reason, .. } => format!("unavailable: {reason}"),
        InjectionOutcome::Delivered => "delivered to caller".into(),
        InjectionOutcome::Skipped { reason } => format!("not injected ({})", reason.as_str()),
        InjectionOutcome::Failed { error } => format!("injection failed: {}", error.message),
        InjectionOutcome::Unknown => "unknown outcome".into(),
    }
}

fn timings(t: &StageTimings) -> String {
    let mut parts = Vec::new();
    for (name, timing) in t.stages() {
        match timing {
            StageTiming::Ran { ms } => parts.push(format!("{name} {ms:.0}ms")),
            StageTiming::Failed { ms, .. } => parts.push(format!("{name} {ms:.0}ms!")),
            // Skipped and not-reported stages are omitted rather than shown as
            // zero — the whole point of the four-state timing.
            StageTiming::Skipped { .. } | StageTiming::NotReported => {}
        }
    }
    match t.total_ms {
        Some(total) => format!("[{} = {total:.0}ms total]", parts.join(" ")),
        None => format!("[{}]", parts.join(" ")),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= max {
        return s;
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn human_duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h{}m", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dictate_proto::{InjectMethod, SkipReason};

    #[test]
    fn awaiting_consent_is_rendered_as_pending_not_as_a_result() {
        let text = injection(&InjectionOutcome::AwaitingConsent {
            backend: "portal".into(),
            consent_id: Some("req-7".into()),
        });
        assert!(text.contains("awaiting"), "{text}");
        assert!(
            !text.contains("injected") && !text.contains("failed"),
            "a pending consent must not read as settled: {text}"
        );
    }

    #[test]
    fn delivered_reads_as_success_not_as_a_skip() {
        let text = injection(&InjectionOutcome::Delivered);
        assert!(text.contains("delivered"));
        assert!(!text.contains("not injected"));
    }

    #[test]
    fn a_skip_names_its_reason() {
        let text = injection(&InjectionOutcome::Skipped {
            reason: SkipReason::NotPermitted,
        });
        assert!(text.contains("not_permitted"), "{text}");
    }

    #[test]
    fn injected_reports_the_method_and_count() {
        let text = injection(&InjectionOutcome::Injected {
            method: InjectMethod::Paste,
            chars: 12,
        });
        assert!(text.contains("12"));
        assert!(text.contains("paste"));
    }

    #[test]
    fn skipped_stages_are_omitted_rather_than_shown_as_zero() {
        let t = StageTimings {
            capture: StageTiming::ran(50.0),
            vad: StageTiming::skipped(SkipReason::NotSupported),
            stt: StageTiming::ran(400.0),
            fmt_rules: StageTiming::NotReported,
            fmt_llm: StageTiming::skipped(SkipReason::BelowMinWords),
            inject: StageTiming::ran(60.0),
            total_ms: Some(540.0),
            audio_ms: Some(2000.0),
        };
        let line = timings(&t);
        assert!(line.contains("capture 50ms"));
        assert!(line.contains("stt 400ms"));
        assert!(line.contains("540ms total"));
        assert!(
            !line.contains("vad") && !line.contains("fmt_llm"),
            "a skipped stage must not appear as a duration: {line}"
        );
    }

    #[test]
    fn a_failed_stage_is_marked_rather_than_hidden() {
        let t = StageTimings {
            fmt_llm: StageTiming::Failed {
                ms: 3000.0,
                error: Some("timeout".into()),
            },
            ..StageTimings::default()
        };
        let line = timings(&t);
        assert!(
            line.contains("fmt_llm 3000ms!"),
            "time burned before a failure stays in the budget: {line}"
        );
    }

    #[test]
    fn truncate_keeps_short_text_intact() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
    }

    #[test]
    fn truncate_is_char_safe_on_multibyte_text() {
        // Byte-slicing here would panic; the count is in characters.
        let s = "héllo wörld ünicode";
        let out = truncate(s, 8);
        assert_eq!(out.chars().count(), 8);
    }

    #[test]
    fn newlines_are_flattened_so_a_row_stays_one_line() {
        assert_eq!(truncate("a\nb", 10), "a b");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_duration(5_000), "5s");
        assert_eq!(human_duration(120_000), "2m");
        assert_eq!(human_duration(7_200_000), "2h0m");
    }

    #[test]
    fn a_broken_formatter_is_spelled_out_in_status() {
        use dictate_proto::{FormatterHealth, FormatterStatus};
        let line = formatter_line(&FormatterStatus {
            enabled: true,
            model: Some("qwen3:14b".into()),
            health: FormatterHealth::ModelMissing,
            detail: Some("installed: gemma4:12b".into()),
        });
        assert!(
            line.contains("MODEL MISSING") && line.contains("UNFORMATTED"),
            "{line}"
        );
        assert!(
            line.contains("dictate doctor") && line.contains("gemma4:12b"),
            "{line}"
        );

        let ok = formatter_line(&FormatterStatus {
            enabled: true,
            model: Some("gemma4:12b".into()),
            health: FormatterHealth::Ok,
            detail: None,
        });
        assert_eq!(ok, "format   gemma4:12b (ok)");
    }

    #[test]
    fn the_microphone_line_says_whether_the_device_is_open_while_idle() {
        use dictate_proto::AudioStatus;
        let line = |capture_enabled, input_open, pre_roll_ms| {
            audio_line(&AudioStatus {
                capture_enabled,
                input_open,
                pre_roll_ms: Some(pre_roll_ms),
            })
        };
        assert!(line(true, true, 300).contains("OPEN while idle"));
        assert!(line(true, false, 0).contains("closed while idle"));
        assert!(line(false, false, 0).contains("audio-less"));
    }
}
