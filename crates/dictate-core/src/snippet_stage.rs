//! S24's snippets as S20's `Slot::Snippets` text stage.
//!
//! A spoken trigger becomes its stored expansion, inserted as a **protected
//! span** (`SpanKind::Snippet`): no later deterministic stage (casing, spacing,
//! terminal punctuation) can touch it and the LLM pass must return it
//! byte-for-byte. It runs after `protect` and the dictionary, so a trigger
//! phrase inside a URL, path or code span is never expanded, and a dictionary
//! term is never mistaken for one.
//!
//! Variables (`{date}`, `{time}`, `{clipboard}`, `{selection}`) are resolved
//! here, lazily, so the clipboard is read only when a matched snippet uses it
//! and only for a session that may read host state
//! ([`FormatContext::host_variables`]).

use std::sync::Arc;

use chrono::Local;
use dictate_dict::{expand_snippet, Dictionary, SnippetVariables};
use dictate_fmt::text::is_placeholder;
use dictate_fmt::{Edit, FormatContext, SpanKind, TextDoc, TextStage};
use tracing::warn;

/// Longest clipboard/selection text pasted into an expansion, in characters.
pub const MAX_VARIABLE_CHARS: usize = 10_000;

/// Applies snippets to the chain's working text.
pub struct SnippetStage {
    dictionary: Arc<Dictionary>,
    variables: Arc<dyn SnippetVariables>,
}

impl SnippetStage {
    /// Wrap a shared dictionary and a source of variable values.
    #[must_use]
    pub fn new(dictionary: Arc<Dictionary>, variables: Arc<dyn SnippetVariables>) -> Self {
        Self {
            dictionary,
            variables,
        }
    }
}

/// Withholds host state from a session that may not read it. The placeholder
/// stays in the output as written, which is honest — an empty string would
/// silently eat it.
struct Gate<'a> {
    inner: &'a dyn SnippetVariables,
    host: bool,
}

impl SnippetVariables for Gate<'_> {
    fn resolve(&self, name: &str) -> Option<String> {
        if matches!(name, "clipboard" | "selection") && !self.host {
            return None;
        }
        self.inner.resolve(name).map(|v| sanitize(&v))
    }
}

/// Clipboard text is arbitrary. The chain reserves private-use characters for
/// its placeholders, and a stray one would make the whole edit batch fail, so
/// they go; so do control characters other than newline and tab.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|&c| !is_placeholder(c) && (!c.is_control() || c == '\n' || c == '\t'))
        .take(MAX_VARIABLE_CHARS)
        .collect()
}

impl TextStage for SnippetStage {
    fn name(&self) -> &'static str {
        "snippets"
    }

    fn apply(&self, doc: &mut TextDoc, ctx: &FormatContext) {
        if !ctx.use_dictionary {
            return;
        }
        // Placeholders are private-use characters, never part of a word, so a
        // trigger cannot match across or inside a protected span.
        let matches = self
            .dictionary
            .find_snippets(doc.working_text(), ctx.app.as_ref());
        if matches.is_empty() {
            return;
        }
        let gate = Gate {
            inner: self.variables.as_ref(),
            host: ctx.host_variables,
        };
        let edits = matches
            .iter()
            .map(|m| {
                Edit::protected(
                    m.range.clone(),
                    expand_snippet(&m.template, &gate),
                    SpanKind::Snippet,
                )
            })
            .collect();
        match doc.apply_edits(edits) {
            // Counted only once the expansions are in the document, and never
            // for a session that must leave no trace.
            Ok(_) => self.dictionary.record_snippet_hits(&matches, !ctx.persist),
            Err(error) => warn!(%error, "snippet edits rejected; text left unchanged"),
        }
    }
}

/// Date, time and — for a session that may read them — the clipboard and the
/// primary selection of the desktop this daemon runs on.
pub struct SystemVariables;

impl SnippetVariables for SystemVariables {
    fn resolve(&self, name: &str) -> Option<String> {
        match name {
            "date" => Some(Local::now().format("%Y-%m-%d").to_string()),
            "time" => Some(Local::now().format("%H:%M").to_string()),
            "clipboard" => Some(read_text(false)),
            "selection" => Some(read_text(true)),
            _ => None,
        }
    }
}

/// An unavailable clipboard (no display, Wayland without a bridge, an empty or
/// non-text selection) is an empty value, never an error: the dictation must
/// still be typed. The content is never logged.
fn read_text(primary: bool) -> String {
    use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind};
    let kind = if primary {
        LinuxClipboardKind::Primary
    } else {
        LinuxClipboardKind::Clipboard
    };
    match Clipboard::new().and_then(|mut c| c.get().clipboard(kind).text()) {
        Ok(text) => text,
        Err(error) => {
            warn!(%error, primary, "snippet variable unavailable; using an empty value");
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary_stage::assemble_text_chain;
    use dictate_fmt::{FormatConfig, Slot, TextChain};
    use dictate_proto::{AppContext, DictionaryEntry, Snippet};

    struct Fixed;
    impl SnippetVariables for Fixed {
        fn resolve(&self, name: &str) -> Option<String> {
            Some(
                match name {
                    "date" => "2026-01-02",
                    "time" => "09:30",
                    "clipboard" => "pasted\u{0}\u{F0001}text",
                    "selection" => "chosen",
                    _ => return None,
                }
                .to_string(),
            )
        }
    }

    fn dictionary() -> Arc<Dictionary> {
        Arc::new(Dictionary::in_memory().expect("in-memory dictionary"))
    }

    fn chain(d: &Arc<Dictionary>) -> TextChain {
        TextChain::standard(&FormatConfig::default()).with_stage(
            Slot::Snippets,
            Box::new(SnippetStage::new(d.clone(), Arc::new(Fixed))),
        )
    }

    fn add(d: &Dictionary, trigger: &str, expansion: &str) {
        d.upsert_snippet(Snippet::new(trigger, expansion))
            .expect("upsert");
    }

    fn live() -> FormatContext {
        FormatContext {
            host_variables: true,
            ..FormatContext::default()
        }
    }

    #[test]
    fn the_expansion_is_verbatim_and_no_rule_touches_it() {
        let d = dictionary();
        // Everything the rules would "fix": lowercase start, a stutter,
        // a spoken number, trailing space, no terminal period.
        add(&d, "sig", "best  regards ,  jake the the 5 \n  ok");
        let run = chain(&d).run("sig", &live());
        assert_eq!(run.doc.restore(), "best  regards ,  jake the the 5 \n  ok");
        let kinds: Vec<_> = run.doc.spans_in_text_order().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![SpanKind::Snippet]);
    }

    #[test]
    fn a_formatter_that_edits_an_expansion_is_rejected_by_the_guard() {
        let d = dictionary();
        add(&d, "work email", "jake@example.com");
        let run = chain(&d).run("send it to work email now", &live());
        assert_eq!(run.doc.restore(), "Send it to jake@example.com now.");
        assert!(run
            .doc
            .verify_output("Send it to Jake@example.com now.")
            .is_err());
        assert!(run.doc.verify_output("Send it to now.").is_err());
    }

    #[test]
    fn a_trigger_inside_protected_text_is_not_expanded() {
        let d = dictionary();
        add(&d, "sig", "SIGNATURE");
        let out = chain(&d).format(
            "open ~/code/sig/main.rs and https://example.com/sig then sig",
            &live(),
        );
        assert_eq!(
            out,
            "Open ~/code/sig/main.rs and https://example.com/sig then SIGNATURE"
        );
    }

    #[test]
    fn a_trigger_swallows_whispers_final_period_but_not_a_mid_sentence_comma() {
        let d = dictionary();
        add(&d, "work email", "jake@example.com");
        let c = chain(&d);
        assert_eq!(c.format("Work email.", &live()), "jake@example.com");
        assert_eq!(
            c.format("write to work email, thanks", &live()),
            "Write to jake@example.com, thanks."
        );
    }

    #[test]
    fn variables_resolve_and_the_value_is_sanitised() {
        let d = dictionary();
        add(&d, "stamp", "{date} {time}");
        add(&d, "paste", "[{clipboard}] [{selection}]");
        let c = chain(&d);
        assert_eq!(c.format("stamp", &live()), "2026-01-02 09:30");
        // NUL and the private-use placeholder never reach the document, and
        // the batch is not rejected because of them.
        assert_eq!(c.format("paste", &live()), "[pastedtext] [chosen]");
    }

    #[test]
    fn host_state_is_withheld_from_a_session_that_may_not_read_it() {
        let d = dictionary();
        add(&d, "paste", "<{clipboard}|{selection}|{date}>");
        let upload = FormatContext::default(); // host_variables = false
        assert_eq!(
            chain(&d).format("paste", &upload),
            "<{clipboard}|{selection}|2026-01-02>",
            "date still resolves; the clipboard placeholder stays visible"
        );
    }

    #[test]
    fn opting_out_expands_nothing_and_privacy_records_no_hits() {
        let d = dictionary();
        add(&d, "sig", "SIGNATURE");
        let off = FormatContext {
            use_dictionary: false,
            ..live()
        };
        assert_eq!(chain(&d).format("sig", &off), "Sig");

        let _ = chain(&d).format("sig", &live()); // persist = false → no hit
        d.flush_hits().unwrap();
        assert_eq!(d.list_snippets(None, None)[0].hit_count, Some(0));
        let persist = FormatContext {
            persist: true,
            ..live()
        };
        let _ = chain(&d).format("sig", &persist);
        d.flush_hits().unwrap();
        assert_eq!(d.list_snippets(None, None)[0].hit_count, Some(1));
    }

    #[test]
    fn per_app_snippets_follow_the_session_context() {
        let d = dictionary();
        d.upsert_snippet(Snippet {
            apps: vec!["slack".into()],
            ..Snippet::new("standup", "yesterday, today, blockers")
        })
        .unwrap();
        let slack = FormatContext {
            app: Some(AppContext::new("Slack")),
            ..live()
        };
        assert_eq!(
            chain(&d).format("standup", &slack),
            "yesterday, today, blockers"
        );
        assert_eq!(chain(&d).format("standup", &live()), "Standup");
    }

    #[test]
    fn dictionary_runs_first_so_its_terms_never_become_triggers() {
        let d = dictionary();
        let mut e = DictionaryEntry::new("Tauri");
        e.sounds_like = vec!["tory".into()];
        d.upsert(e).unwrap();
        add(&d, "tory", "SNIPPET");
        let c = assemble_text_chain(&FormatConfig::default(), Some(&d));
        assert_eq!(c.format("use tory", &live()), "Use Tauri");
    }

    #[test]
    fn the_assembled_chain_orders_snippets_after_the_dictionary() {
        let d = dictionary();
        let names = assemble_text_chain(&FormatConfig::default(), Some(&d)).stage_names();
        let dict = names.iter().position(|n| *n == "dictionary").unwrap();
        let snip = names.iter().position(|n| *n == "snippets").unwrap();
        assert_eq!(snip, dict + 1);
        assert!(names
            .iter()
            .position(|n| *n == "snippets")
            .is_some_and(|i| i < names.len() - 1));
        let without = assemble_text_chain(&FormatConfig::default(), None).stage_names();
        assert!(!without.contains(&"snippets"));
    }

    #[test]
    fn many_snippets_in_one_utterance_all_expand() {
        let d = dictionary();
        add(&d, "one", "1");
        add(&d, "two", "2");
        let run = chain(&d).run("one and two and one", &live());
        assert_eq!(run.doc.restore(), "1 and 2 and 1");
        assert_eq!(run.doc.spans().len(), 3);
    }
}
