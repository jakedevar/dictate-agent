//! S22's personal dictionary as S20's `Slot::Dictionary` text stage.
//!
//! The dictionary runs *inside* the deterministic chain — after `protect` and
//! the built-in corrections, before snippets and the rules (integration
//! contract §1) — so URLs, paths and code are never rewritten by an alias,
//! and every canonical term it produces becomes a protected span that the
//! casing rules cannot touch and the LLM pass must return byte-for-byte.
//!
//! The adapter lives here because `dictate-fmt` and `dictate-dict` are both
//! leaves: neither may depend on the other, and `dictate-core` already
//! depends on both.

use std::sync::Arc;

use dictate_dict::Dictionary;
use dictate_fmt::{
    Edit, FormatConfig, FormatContext, Slot, SpanKind, TextChain, TextDoc, TextStage,
};
use tracing::warn;

/// Applies the dictionary to the chain's working text.
pub struct DictionaryStage {
    dictionary: Arc<Dictionary>,
}

impl DictionaryStage {
    /// Wrap a shared dictionary.
    #[must_use]
    pub fn new(dictionary: Arc<Dictionary>) -> Self {
        Self { dictionary }
    }
}

impl TextStage for DictionaryStage {
    fn name(&self) -> &'static str {
        "dictionary"
    }

    fn apply(&self, doc: &mut TextDoc, ctx: &FormatContext) {
        if !ctx.use_dictionary {
            return;
        }
        // Placeholders (protected spans) are private-use characters, which the
        // matcher treats as word boundaries: an alias can never match across
        // or inside one.
        let applied = self.dictionary.apply(doc.working_text(), ctx.app.as_ref());
        if applied.replacements.is_empty() {
            return;
        }
        let mut replacements = applied.replacements;
        replacements.sort_by_key(|r| r.range.start);

        // The matcher reports ranges in its *output*; edits address the text
        // it was given. Walk the replacements in order, undoing the length
        // change of each earlier one.
        let mut shift: isize = 0;
        let mut edits = Vec::with_capacity(replacements.len());
        for r in &replacements {
            let Some(start) = r.range.start.checked_add_signed(-shift) else {
                warn!("dictionary replacement out of range; leaving the text unchanged");
                return;
            };
            let end = start + r.before.len();
            edits.push(Edit::protected(start..end, r.after.clone(), SpanKind::Term));
            shift += r.after.len() as isize - r.before.len() as isize;
        }
        match doc.apply_edits(edits) {
            // Counted only once the terms are actually in the document, and
            // never for a session that must leave no trace.
            Ok(_) => self.dictionary.record_hits(&replacements, !ctx.persist),
            Err(error) => warn!(%error, "dictionary edits rejected; text left unchanged"),
        }
    }
}

/// The daemon's text chain: the configured rules, with the dictionary in its
/// slot when one is available. Every pipeline — the daemon's, the test
/// harness's — should be built through this so a dictionary is never present
/// on the `Pipeline` while silently absent from the chain.
#[must_use]
pub fn assemble_text_chain(
    format: &FormatConfig,
    dictionary: Option<&Arc<Dictionary>>,
) -> TextChain {
    let chain = TextChain::standard(format);
    match dictionary {
        // Snippets (S24) live in the same database and handle, and run right
        // after the dictionary (contract §1): one assembly, so neither is ever
        // present on the `Pipeline` yet missing from the chain.
        Some(dictionary) => chain
            .with_stage(
                Slot::Dictionary,
                Box::new(DictionaryStage::new(dictionary.clone())),
            )
            .with_stage(
                Slot::Snippets,
                Box::new(crate::snippet_stage::SnippetStage::new(
                    dictionary.clone(),
                    Arc::new(crate::snippet_stage::SystemVariables),
                )),
            ),
        None => chain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dictate_proto::DictionaryEntry;

    fn dictionary(entries: &[(&str, &[&str])]) -> Arc<Dictionary> {
        let dictionary = Arc::new(Dictionary::in_memory().expect("in-memory dictionary"));
        for (phrase, aliases) in entries {
            let mut e = DictionaryEntry::new(*phrase);
            e.sounds_like = aliases.iter().map(|a| (*a).to_string()).collect();
            dictionary.upsert(e).expect("upsert");
        }
        dictionary
    }

    fn chain(d: &Arc<Dictionary>) -> TextChain {
        assemble_text_chain(&FormatConfig::default(), Some(d))
    }

    #[test]
    fn replacements_of_different_lengths_land_in_the_right_places_and_are_protected() {
        let d = dictionary(&[("Kubernetes", &["cube ernetties"]), ("Tauri", &["tory"])]);
        let run = chain(&d).run(
            "deploy cube ernetties with tory and cube ernetties again",
            &FormatContext::default(),
        );
        assert_eq!(
            run.doc.restore(),
            "Deploy Kubernetes with Tauri and Kubernetes again."
        );
        let terms: Vec<_> = run
            .doc
            .spans()
            .iter()
            .filter(|s| s.kind == SpanKind::Term)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(terms, vec!["Kubernetes", "Tauri", "Kubernetes"]);
        // A formatter that "fixes" a canonical term is rejected by the guard.
        assert!(run
            .doc
            .verify_output("Deploy kubernetes with Tauri and Kubernetes again.")
            .is_err());
    }

    #[test]
    fn the_dictionary_never_rewrites_inside_a_protected_span() {
        let d = dictionary(&[("Tauri", &["tory"])]);
        let out = chain(&d).format(
            "see https://example.com/tory and ~/code/tory/main.rs and tory",
            &FormatContext::default(),
        );
        assert_eq!(
            out,
            "See https://example.com/tory and ~/code/tory/main.rs and Tauri."
        );
    }

    #[test]
    fn opting_out_disables_the_stage_and_privacy_records_no_hits() {
        let d = dictionary(&[("Tauri", &["tory"])]);
        let off = FormatContext {
            use_dictionary: false,
            ..FormatContext::default()
        };
        assert_eq!(chain(&d).format("use tory", &off), "Use tory");

        // Default context: persist = false (privacy by default) → no hits.
        let _ = chain(&d).format("use tory", &FormatContext::default());
        d.flush_hits().unwrap();
        assert_eq!(d.list(None, None)[0].hit_count, Some(0));

        let persist = FormatContext {
            persist: true,
            ..FormatContext::default()
        };
        let _ = chain(&d).format("use tory", &persist);
        d.flush_hits().unwrap();
        assert_eq!(d.list(None, None)[0].hit_count, Some(1));
    }

    #[test]
    fn corrections_run_before_the_dictionary_and_both_outputs_stay_protected() {
        let d = dictionary(&[("Tauri", &["tory"])]);
        let run = chain(&d).run("create plan for building tory", &FormatContext::default());
        // Four words, ending on a dictionary term: the sentence is closed.
        assert_eq!(run.doc.restore(), "/create_plan for building Tauri.");
        let kinds: Vec<_> = run.doc.spans_in_text_order().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![SpanKind::SlashCommand, SpanKind::Term]);
        // Three words stay bare, exactly as for plain words.
        assert_eq!(
            chain(&d).format("create plan for tory", &FormatContext::default()),
            "/create_plan for Tauri"
        );
    }
}
