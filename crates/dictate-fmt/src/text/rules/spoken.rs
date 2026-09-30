//! `spoken_punctuation` and `spoken_line_breaks` (both off by default).
//!
//! Jake dictates code prompts in which "new line", "period" and "colon" are
//! often literal words, so these only run where a profile (S23) turns them
//! on. When on, a spoken mark replaces any punctuation Whisper guessed at the
//! same spot ("Hello, comma world" → "Hello, world", "done period." →
//! "done.") and attaches to the right neighbour: closing marks to the word
//! before, opening marks to the word after.

use crate::text::lex::{Editor, Kind};
use crate::text::{FormatContext, TextDoc, TextStage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attach {
    /// `,` `.` `?` … — no space before.
    Close,
    /// `(` and an opening quote — no space after.
    Open,
    /// A line break — no space on either side.
    Break,
}

type Phrase = (&'static [&'static str], &'static str, Attach);

const PUNCTUATION: &[Phrase] = &[
    (&["exclamation", "point"], "!", Attach::Close),
    (&["exclamation", "mark"], "!", Attach::Close),
    (&["question", "mark"], "?", Attach::Close),
    (&["full", "stop"], ".", Attach::Close),
    (&["semi", "colon"], ";", Attach::Close),
    (&["semicolon"], ";", Attach::Close),
    (&["open", "quote"], "\"", Attach::Open),
    (&["close", "quote"], "\"", Attach::Close),
    (&["end", "quote"], "\"", Attach::Close),
    (&["unquote"], "\"", Attach::Close),
    (&["open", "parenthesis"], "(", Attach::Open),
    (&["open", "paren"], "(", Attach::Open),
    (&["close", "parenthesis"], ")", Attach::Close),
    (&["close", "paren"], ")", Attach::Close),
    (&["comma"], ",", Attach::Close),
    (&["period"], ".", Attach::Close),
    (&["colon"], ":", Attach::Close),
];

const LINE_BREAKS: &[Phrase] = &[
    (&["new", "paragraph"], "\n\n", Attach::Break),
    (&["new", "line"], "\n", Attach::Break),
    (&["newline"], "\n", Attach::Break),
];

/// "comma" → `,`, "question mark" → `?`, "open paren" → `(`, …
#[derive(Debug, Default, Clone, Copy)]
pub struct SpokenPunctuation;

impl TextStage for SpokenPunctuation {
    fn name(&self) -> &'static str {
        "spoken_punctuation"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        doc.edit(|ed| replace_phrases(ed, PUNCTUATION));
    }
}

/// "new line" → `\n`, "new paragraph" → `\n\n`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpokenLineBreaks;

impl TextStage for SpokenLineBreaks {
    fn name(&self) -> &'static str {
        "spoken_line_breaks"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        doc.edit(|ed| replace_phrases(ed, LINE_BREAKS));
    }
}

/// Whisper's own guess at punctuation next to a spoken mark.
fn is_guessed_mark(s: &str) -> bool {
    matches!(s, "," | "." | "!" | "?" | ";" | ":")
}

fn match_phrase(ed: &Editor<'_>, i: usize, words: &[&str]) -> Option<(Vec<usize>, usize)> {
    if !ed.text(i).eq_ignore_ascii_case(words[0]) {
        return None;
    }
    let mut used = Vec::new();
    let mut cur = i;
    for w in &words[1..] {
        let (sp, j) = ed.next_word_after_space(cur)?;
        if !ed.text(j).eq_ignore_ascii_case(w) {
            return None;
        }
        used.extend([sp, j]);
        cur = j;
    }
    Some((used, cur))
}

fn replace_phrases(ed: &mut Editor<'_>, phrases: &[Phrase]) {
    let n = ed.len();
    let mut i = 0;
    while i < n {
        if !ed.is_word(i) {
            i += 1;
            continue;
        }
        let Some((used, last, symbol, attach)) =
            phrases.iter().find_map(|(words, symbol, attach)| {
                match_phrase(ed, i, words).map(|(used, last)| (used, last, *symbol, *attach))
            })
        else {
            i += 1;
            continue;
        };
        for t in used {
            ed.delete(t);
        }
        // Absorb the mark Whisper guessed right after the phrase.
        let mut end = last;
        if let Some(p) = ed.touching_next(last) {
            if ed.kind(p) == Kind::Punct && is_guessed_mark(ed.text(p)) {
                ed.delete(p);
                end = p;
            }
        }
        if matches!(attach, Attach::Close | Attach::Break) {
            if let Some(sp) = ed.prev_alive(i).filter(|&s| ed.kind(s) == Kind::Space) {
                ed.delete(sp);
                // "Hello, comma" — the spoken mark replaces Whisper's guess.
                if attach == Attach::Close {
                    if let Some(g) = ed
                        .prev_alive(sp)
                        .filter(|&g| ed.kind(g) == Kind::Punct && matches!(ed.text(g), "," | "."))
                    {
                        ed.delete(g);
                    }
                }
            }
        }
        if matches!(attach, Attach::Open | Attach::Break) {
            if let Some(sp) = ed.next_alive(end).filter(|&s| ed.kind(s) == Kind::Space) {
                ed.delete(sp);
            }
        }
        ed.replace(i, symbol.to_string());
        i = end + 1;
    }
}

#[cfg(test)]
mod tests {
    use crate::text::rules::test_support::stage;

    use super::*;

    #[test]
    fn spoken_punctuation() {
        let cases: &[(&str, &str)] = &[
            ("hello comma world", "hello, world"),
            ("Hello, comma world", "Hello, world"),
            ("is it done question mark", "is it done?"),
            ("stop exclamation point", "stop!"),
            ("wow exclamation mark", "wow!"),
            ("done period next", "done. next"),
            ("done period.", "done."),
            ("done full stop", "done."),
            ("note colon this", "note: this"),
            ("one semicolon two", "one; two"),
            (
                "he said open quote hi close quote twice",
                "he said \"hi\" twice",
            ),
            (
                "call it open paren later close paren now",
                "call it (later) now",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(stage(&SpokenPunctuation, input), *want, "input: {input:?}");
        }
    }

    #[test]
    fn spoken_line_breaks() {
        let cases: &[(&str, &str)] = &[
            ("first item new line second item", "first item\nsecond item"),
            (
                "First item. New line. Second item.",
                "First item.\nSecond item.",
            ),
            ("intro new paragraph body", "intro\n\nbody"),
            ("a newline b", "a\nb"),
        ];
        for (input, want) in cases {
            assert_eq!(stage(&SpokenLineBreaks, input), *want, "input: {input:?}");
        }
    }

    #[test]
    fn whole_words_only() {
        for input in [
            "use commas here",
            "a periodic job",
            "the colonel",
            "a new liner",
        ] {
            assert_eq!(stage(&SpokenPunctuation, input), input);
            assert_eq!(stage(&SpokenLineBreaks, input), input);
        }
    }
}
