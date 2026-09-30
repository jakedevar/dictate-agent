//! `terminal_punctuation`: a closing period on complete utterances.
//!
//! Appends `.` when the utterance has at least four words, ends in a letter or
//! digit, and does not end with a protected span (`… then run /create_plan`
//! must not become `/create_plan.` in a terminal). Short utterances are often
//! commands or fragments ("git status", "timer 10 minutes") and are left bare.

use crate::text::lex::{Editor, Kind};
use crate::text::{FormatContext, TextDoc, TextStage};

/// Minimum words before a period is added.
pub const MIN_WORDS: usize = 4;

/// Adds a final period to complete utterances.
#[derive(Debug, Default, Clone, Copy)]
pub struct TerminalPunctuation;

impl TextStage for TerminalPunctuation {
    fn name(&self) -> &'static str {
        "terminal_punctuation"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        doc.edit(terminate);
    }
}

fn terminate(ed: &mut Editor<'_>) {
    let words = (0..ed.len())
        .filter(|&i| ed.alive(i) && matches!(ed.kind(i), Kind::Word | Kind::Protected))
        .count();
    if words < MIN_WORDS {
        return;
    }
    let Some(last) = (0..ed.len())
        .rev()
        .find(|&i| ed.alive(i) && !matches!(ed.kind(i), Kind::Space | Kind::Newline))
    else {
        return;
    };
    if ed.kind(last) != Kind::Word {
        return;
    }
    let text = ed.text(last);
    if text.chars().next_back().is_some_and(char::is_alphanumeric) {
        let with_period = format!("{text}.");
        ed.replace(last, with_period);
    }
}

#[cfg(test)]
mod tests {
    use crate::text::rules::test_support::{stage, stage_after_protect};

    use super::*;

    #[test]
    fn terminates_complete_utterances_only() {
        let cases: &[(&str, &str)] = &[
            ("we need to fix this", "we need to fix this."),
            ("the answer is 42", "the answer is 42."),
            ("is this the right one", "is this the right one."),
            ("git status", "git status"),
            ("timer 10 minutes", "timer 10 minutes"),
            ("three words only", "three words only"),
            ("already done here.", "already done here."),
            ("is it done yet?", "is it done yet?"),
            ("wait for it...", "wait for it..."),
            ("he said \"go now\"", "he said \"go now\""),
            ("list the items:", "list the items:"),
        ];
        for (input, want) in cases {
            assert_eq!(
                stage(&TerminalPunctuation, input),
                *want,
                "input: {input:?}"
            );
        }
    }

    #[test]
    fn never_after_a_protected_span() {
        for input in [
            "after research run /create_plan",
            "please open the file ~/.claude/settings.json",
            "send it to jane@example.com",
            "check the value of user_id",
        ] {
            assert_eq!(
                stage_after_protect(&TerminalPunctuation, input),
                input,
                "input: {input:?}"
            );
        }
    }
}
