//! The built-in deterministic rules, one [`TextStage`](super::TextStage) each.
//!
//! Each rule is conservative by default and never changes what the speaker
//! meant; each has table-driven tests with must-not-change negatives next to
//! it. Every rule edits through the token editor in `lex.rs`, which refuses
//! to touch a protected span.

mod casing;
mod corrections;
mod fillers;
mod numbers;
mod scrub;
mod spacing;
mod spoken;
mod stutters;
mod terminal;

pub use casing::Casing;
pub use corrections::BuiltinCorrections;
pub use fillers::Fillers;
pub use numbers::Numbers;
pub use scrub::HallucinationScrub;
pub use spacing::Spacing;
pub use spoken::{SpokenLineBreaks, SpokenPunctuation};
pub use stutters::Stutters;
pub use terminal::{TerminalPunctuation, MIN_WORDS as TERMINAL_MIN_WORDS};

/// Apply the built-in dot-directory fix to a path (`~/.cloud/x` →
/// `~/.claude/x`); identity for anything else. Exposed so corpus properties
/// can predict how a detected path span legitimately changes.
#[must_use]
pub fn corrected_path(path: &str) -> String {
    corrections::fix_dot_dirs(path).unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::text::{ChainRun, FormatContext, Protect, StageTimes, TextDoc, TextStage};

    /// Run one stage on unprotected text.
    pub(crate) fn stage(stage: &dyn TextStage, input: &str) -> String {
        let mut doc = TextDoc::new(input);
        stage.apply(&mut doc, &FormatContext::default());
        doc.restore()
    }

    /// Run `protect`, then one stage.
    pub(crate) fn stage_after_protect(stage: &dyn TextStage, input: &str) -> String {
        chain_with(&[stage], input).doc.restore()
    }

    /// Run `protect`, then `stages` in order.
    pub(crate) fn chain_with(stages: &[&dyn TextStage], input: &str) -> ChainRun {
        let ctx = FormatContext::default();
        let mut doc = TextDoc::new(input);
        Protect.apply(&mut doc, &ctx);
        for s in stages {
            s.apply(&mut doc, &ctx);
        }
        ChainRun {
            doc,
            timings: StageTimes::default(),
        }
    }
}
