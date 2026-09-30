//! `casing`: sentence-start capitals and the pronoun "I".
//!
//! Only ever raises a letter; never lowercases. A sentence starts at the
//! beginning of the text, after a line break, or after `.`/`!`/`?` and a
//! space — not after an abbreviation (`e.g.`, `i.e.`, `vs.`, `etc.`, `a.m.`)
//! or an ellipsis, and never inside or right after a protected span
//! (`/research_codebase for the auth flow` keeps its lowercase `for`).
//!
//! The pronoun: a standalone `i` and `i'm`/`i've`/`i'll`/`i'd` become `I…`,
//! except where `i` is plainly a loop variable ("for i in range", "while i is
//! less than n", "i equals zero") — Jake dictates code prompts, and Whisper
//! already capitalizes the pronoun itself.

use crate::text::lex::{capitalized, Editor, Kind};
use crate::text::{FormatContext, TextDoc, TextStage};

/// Capitalizes sentence starts and the pronoun "I".
#[derive(Debug, Default, Clone, Copy)]
pub struct Casing;

impl TextStage for Casing {
    fn name(&self) -> &'static str {
        "casing"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        doc.edit(case);
    }
}

/// Words before `i` that make it a variable: "for i", "let i", "index i".
const VARIABLE_BEFORE: &[&str] = &[
    "for", "let", "var", "int", "index", "variable", "counter", "loop", "each", "const", "mut",
];
/// Words after `i` that make it a variable: "i in", "i is", "i equals".
const VARIABLE_AFTER: &[&str] = &[
    "in",
    "is",
    "equals",
    "plus",
    "minus",
    "times",
    "from",
    "starts",
    "increments",
    "goes",
];
const CONTRACTIONS: &[&str] = &[
    "i'm",
    "i've",
    "i'll",
    "i'd",
    "i\u{2019}m",
    "i\u{2019}ve",
    "i\u{2019}ll",
    "i\u{2019}d",
];

fn is_variable_i(ed: &Editor<'_>, i: usize) -> bool {
    // `i.e.`
    if ed.touching_next(i).is_some_and(|d| {
        ed.text(d) == "."
            && ed
                .touching_next(d)
                .is_some_and(|w| ed.kind(w) == Kind::Word)
    }) {
        return true;
    }
    let before = ed
        .prev_solid(i)
        .filter(|&p| ed.kind(p) == Kind::Word)
        .is_some_and(|p| {
            VARIABLE_BEFORE
                .iter()
                .any(|v| v.eq_ignore_ascii_case(ed.text(p)))
        });
    let after = ed.next_solid(i).is_some_and(|n| match ed.kind(n) {
        Kind::Word => VARIABLE_AFTER
            .iter()
            .any(|v| v.eq_ignore_ascii_case(ed.text(n))),
        Kind::Punct => matches!(
            ed.text(n),
            "=" | "<" | ">" | "+" | "*" | "/" | "[" | "]" | ")" | "-" | "%"
        ),
        _ => false,
    });
    before || after
}

fn case(ed: &mut Editor<'_>) {
    for i in 0..ed.len() {
        if !ed.is_word(i) {
            continue;
        }
        let word = ed.text(i);
        if word == "i" {
            if !is_variable_i(ed, i) {
                ed.replace(i, "I".to_string());
            }
            continue;
        }
        if CONTRACTIONS.contains(&word) {
            let up = capitalized(word).expect("starts with a lowercase i");
            ed.replace(i, up);
            continue;
        }
        if ed.at_sentence_start(i) {
            if let Some(up) = capitalized(word) {
                ed.replace(i, up);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::text::rules::test_support::{stage, stage_after_protect};

    use super::*;

    #[test]
    fn capitalizes_sentence_starts_and_the_pronoun() {
        let cases: &[(&str, &str)] = &[
            ("hello there", "Hello there"),
            ("done. next step", "Done. Next step"),
            ("really? yes! ok", "Really? Yes! Ok"),
            ("line one\nline two", "Line one\nLine two"),
            ("he said \"stop.\" then left", "He said \"stop.\" Then left"),
            ("\"quoted start\" here", "\"Quoted start\" here"),
            ("(see below) now", "(See below) now"),
            (
                "and i think i'm right, i've checked",
                "And I think I'm right, I've checked",
            ),
            ("you and i", "You and I"),
            ("i'll do it and i'd say so", "I'll do it and I'd say so"),
            ("émile arrived. élan too", "Émile arrived. Élan too"),
        ];
        for (input, want) in cases {
            assert_eq!(stage(&Casing, input), *want, "input: {input:?}");
        }
    }

    #[test]
    fn never_capitalizes_where_it_would_change_things() {
        for input in [
            "Use e.g. this one",
            "Pick one, i.e. the first",
            "Apples vs. oranges",
            "Bring pens, paper, etc. and more",
            "At 5 p.m. we leave",
            "Wait... maybe not",
            "The file ends.with a dot",
            "Version 3.5 is out",
            "Loop for i in range",
            "While i is less than n",
            "Set i equals zero",
            "Then i = i + 1",
            "Use array[i] here",
            "It is: lowercase after a colon",
        ] {
            assert_eq!(stage(&Casing, input), input, "input: {input:?}");
        }
    }

    #[test]
    fn protected_spans_are_never_capitalized_and_do_not_start_sentences() {
        assert_eq!(
            stage_after_protect(&Casing, "/research_codebase for the auth flow"),
            "/research_codebase for the auth flow"
        );
        assert_eq!(
            stage_after_protect(&Casing, "done. user_id is next"),
            "Done. user_id is next"
        );
        assert_eq!(
            stage_after_protect(&Casing, "see ~/.claude/x. then go"),
            "See ~/.claude/x. Then go"
        );
    }
}
