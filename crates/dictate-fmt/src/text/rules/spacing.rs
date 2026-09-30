//! `spacing`: whitespace and punctuation spacing.
//!
//! - runs of spaces/tabs → one space; no space at either end or around a
//!   line break;
//! - no space before `, ; ! ?`, nor before `. : …` when they end a word
//!   (`the .env file` and ` :wq` are not sentence punctuation);
//! - one space after `, ;` between two words, and after `? !` before a word
//!   (never after `.` or `:` — `config.yaml`, `std::fs`, `3.14`, `12:30`);
//! - `,,` → `,`; `..` → `.` at a word end (keeps `...` and Rust's `0..10`);
//!   a comma directly before `. ? !` is dropped;
//! - an orphaned comma at the start of the text, a line, or a sentence
//!   (left behind by filler removal) is dropped.
//!
//! Protected spans are single placeholder tokens, so nothing here can reach
//! inside a URL, path or identifier.

use crate::text::lex::{is_terminal, Editor, Kind};
use crate::text::{FormatContext, TextDoc, TextStage};

/// Normalizes spacing around punctuation.
#[derive(Debug, Default, Clone, Copy)]
pub struct Spacing;

impl TextStage for Spacing {
    fn name(&self) -> &'static str {
        "spacing"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        doc.edit(space);
    }
}

fn ends_word(ed: &Editor<'_>, p: usize) -> bool {
    ed.next_alive(p).is_none_or(|n| {
        matches!(ed.kind(n), Kind::Space | Kind::Newline)
            || (ed.kind(n) == Kind::Punct
                && matches!(ed.text(n), "\"" | "'" | ")" | "]" | "\u{201D}" | "\u{2019}"))
    })
}

fn word_ends_in_letter(ed: &Editor<'_>, w: usize) -> bool {
    ed.kind(w) == Kind::Word
        && ed
            .text(w)
            .chars()
            .next_back()
            .is_some_and(char::is_alphabetic)
}

fn word_starts_with_letter(ed: &Editor<'_>, w: usize) -> bool {
    ed.kind(w) == Kind::Word && ed.text(w).chars().next().is_some_and(char::is_alphabetic)
}

fn space(ed: &mut Editor<'_>) {
    let n = ed.len();
    for i in 0..n {
        if !ed.alive(i) {
            continue;
        }
        match ed.kind(i) {
            Kind::Space => {
                let prev = ed.prev_alive(i);
                let next = ed.next_alive(i);
                let edge = prev.is_none()
                    || next.is_none()
                    || prev.is_some_and(|p| ed.kind(p) == Kind::Newline)
                    || next.is_some_and(|x| ed.kind(x) == Kind::Newline);
                let before_punct = next.is_some_and(|x| {
                    ed.kind(x) == Kind::Punct
                        && match ed.text(x) {
                            "," | ";" | "!" | "?" => true,
                            "." | ":" | "..." | "\u{2026}" => ends_word(ed, x),
                            _ => false,
                        }
                });
                if edge || before_punct {
                    ed.delete(i);
                } else if ed.text(i) != " " {
                    ed.replace(i, " ".to_string());
                }
            }
            Kind::Punct => punct(ed, i),
            _ => {}
        }
    }
}

fn punct(ed: &mut Editor<'_>, i: usize) {
    let text = ed.text(i).to_string();
    // Orphaned leading comma.
    if text == "," && orphaned(ed, i) {
        ed.delete(i);
        return;
    }
    // `,,` → `,`
    if text == "," {
        if let Some(n) = ed.touching_next(i).filter(|&n| ed.text(n) == ",") {
            ed.delete(n);
        }
        // `, .` / `,.` → `.`
        let after = match ed.next_alive(i) {
            Some(s) if ed.kind(s) == Kind::Space => ed.next_alive(s),
            other => other,
        };
        if after.is_some_and(|a| ed.kind(a) == Kind::Punct && is_terminal(ed.text(a))) {
            ed.delete(i);
            return;
        }
    }
    // `..` at a word end → `.`
    if text == ".." && ends_word(ed, i) && ed.touching_prev(i).is_some() {
        ed.replace(i, ".".to_string());
        return;
    }
    // One space after `,`/`;` between words, after `?`/`!` before a word.
    let needs_space = match text.as_str() {
        "," | ";" => {
            ed.touching_prev(i)
                .is_some_and(|p| word_ends_in_letter(ed, p))
                && ed
                    .touching_next(i)
                    .is_some_and(|n| word_starts_with_letter(ed, n))
        }
        "?" | "!" => {
            ed.touching_prev(i)
                .is_some_and(|p| ed.kind(p) == Kind::Word)
                && ed
                    .touching_next(i)
                    .is_some_and(|n| word_starts_with_letter(ed, n))
        }
        _ => false,
    };
    if needs_space {
        ed.replace(i, format!("{text} "));
    }
}

/// A comma with nothing before it on its line or in its sentence.
fn orphaned(ed: &Editor<'_>, i: usize) -> bool {
    match ed.prev_solid(i) {
        None => true,
        Some(p) => {
            ed.kind(p) == Kind::Newline || (ed.kind(p) == Kind::Punct && is_terminal(ed.text(p)))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::text::rules::test_support::{stage, stage_after_protect};

    use super::*;

    #[test]
    fn normalizes_spacing() {
        let cases: &[(&str, &str)] = &[
            ("hello   world", "hello world"),
            ("  padded  ", "padded"),
            ("tab\there", "tab here"),
            ("hello , world", "hello, world"),
            ("really ?", "really?"),
            ("stop !", "stop!"),
            ("the end .", "the end."),
            ("edit : make it better", "edit: make it better"),
            ("wait ...", "wait..."),
            ("apples,oranges", "apples, oranges"),
            ("one;two", "one; two"),
            ("really?yes", "really? yes"),
            ("wow!Great", "wow! Great"),
            ("a,, b", "a, b"),
            ("done..", "done."),
            ("done.. next", "done. next"),
            ("the end, .", "the end."),
            ("the end,.", "the end."),
            ("is it, ?", "is it?"),
            (", so we start", "so we start"),
            ("First.\n, second", "First.\nsecond"),
            ("First. , second", "First. second"),
            ("line one  \n  line two", "line one\nline two"),
        ];
        for (input, want) in cases {
            assert_eq!(stage(&Spacing, input), *want, "input: {input:?}");
        }
    }

    #[test]
    fn leaves_code_like_punctuation_alone() {
        for input in [
            "wait... what",
            "range 0..10 here",
            "pi is 3.14",
            "at 12:30 today",
            "1,000 items",
            "the .5 case",
            "type :wq to quit",
            "a (parenthetical) note",
            "he said \"hi\" twice",
            "a - b",
            "Mr. Smith",
        ] {
            assert_eq!(stage(&Spacing, input), input, "input: {input:?}");
        }
    }

    #[test]
    fn never_touches_protected_spans() {
        assert_eq!(
            stage_after_protect(
                &Spacing,
                "open  config.yaml , then https://a.io/x?y=1,2 now"
            ),
            "open config.yaml, then https://a.io/x?y=1,2 now"
        );
        assert_eq!(
            stage_after_protect(&Spacing, "the .env file"),
            "the .env file"
        );
    }
}
