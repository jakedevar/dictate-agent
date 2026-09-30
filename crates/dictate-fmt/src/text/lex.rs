//! Tokenizer and token editor shared by the built-in rules.
//!
//! Every rule is one linear pass: lex the working text, mark tokens dead or
//! give them replacement text, serialize. The editor owns the two invariants
//! rules would otherwise each have to get right:
//!
//! - **Protected tokens are immutable.** [`Editor::delete`] and
//!   [`Editor::replace`] refuse a placeholder token (and `debug_assert!` so a
//!   rule bug fails tests loudly).
//! - **Deletions do not leave spacing debris.** [`Editor::finish`] merges the
//!   spaces around a deleted token and drops a space a deletion left before
//!   closing punctuation or a line break, so each rule only deletes the words
//!   it means to.

use super::doc::{is_placeholder, ProtectedSpan, SpanKind, MAX_PROTECTED_SPANS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Letters/digits, with internal apostrophes and hyphens (`don't`,
    /// `twenty-five`) and digit-internal `.`/`,` (`3.14`, `5,000`).
    Word,
    /// One placeholder character.
    Protected,
    /// A run of non-newline whitespace.
    Space,
    /// A run of `\n`.
    Newline,
    /// One punctuation/symbol character, or a run of `.`.
    Punct,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Tok {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
}

fn char_at(text: &str, i: usize) -> Option<char> {
    text.get(i..).and_then(|s| s.chars().next())
}

pub(crate) fn lex(text: &str) -> Vec<Tok> {
    let mut toks = Vec::with_capacity(text.len() / 3 + 1);
    let mut it = text.char_indices().peekable();
    while let Some((start, c)) = it.next() {
        let mut end = start + c.len_utf8();
        let kind = if is_placeholder(c) {
            Kind::Protected
        } else if c == '\n' {
            while let Some(&(i, '\n')) = it.peek() {
                end = i + 1;
                it.next();
            }
            Kind::Newline
        } else if c.is_whitespace() {
            while let Some(&(i, d)) = it.peek() {
                if d.is_whitespace() && d != '\n' {
                    end = i + d.len_utf8();
                    it.next();
                } else {
                    break;
                }
            }
            Kind::Space
        } else if c.is_alphanumeric() {
            let mut prev = c;
            while let Some(&(i, d)) = it.peek() {
                if d.is_alphanumeric() {
                    end = i + d.len_utf8();
                    prev = d;
                    it.next();
                    continue;
                }
                let after = char_at(text, i + d.len_utf8());
                let joins = match d {
                    '\'' | '\u{2019}' | '-' => after.is_some_and(char::is_alphanumeric),
                    '.' | ',' => prev.is_ascii_digit() && after.is_some_and(|a| a.is_ascii_digit()),
                    _ => false,
                };
                if !joins {
                    break;
                }
                it.next();
                let (j, n) = it.next().expect("joiner is followed by a character");
                end = j + n.len_utf8();
                prev = n;
            }
            Kind::Word
        } else if c == '.' {
            while let Some(&(i, '.')) = it.peek() {
                end = i + 1;
                it.next();
            }
            Kind::Punct
        } else {
            Kind::Punct
        };
        toks.push(Tok { kind, start, end });
    }
    toks
}

/// Closing punctuation: a space before it is debris when a deletion put it there.
pub(crate) fn is_closing(s: &str) -> bool {
    matches!(
        s.chars().next(),
        Some(',' | '.' | '!' | '?' | ';' | ':' | ')' | ']' | '}' | '\u{2026}')
    )
}

fn is_opening(s: &str) -> bool {
    matches!(s, "\"" | "'" | "(" | "[" | "\u{201C}" | "\u{2018}")
}

fn is_closing_quote(s: &str) -> bool {
    matches!(s, "\"" | "'" | "\u{201D}" | "\u{2019}" | ")" | "]")
}

/// Sentence-ending punctuation token (a single `.`, `!`, `?`).
pub(crate) fn is_terminal(s: &str) -> bool {
    matches!(s, "." | "!" | "?")
}

pub(crate) struct Editor<'a> {
    src: &'a str,
    toks: Vec<Tok>,
    alive: Vec<bool>,
    repl: Vec<Option<String>>,
    spans: &'a mut Vec<ProtectedSpan>,
    changed: bool,
}

impl<'a> Editor<'a> {
    pub(crate) fn new(src: &'a str, spans: &'a mut Vec<ProtectedSpan>) -> Self {
        let toks = lex(src);
        let n = toks.len();
        Self {
            src,
            toks,
            alive: vec![true; n],
            repl: vec![None; n],
            spans,
            changed: false,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.toks.len()
    }

    pub(crate) fn kind(&self, i: usize) -> Kind {
        self.toks[i].kind
    }

    pub(crate) fn alive(&self, i: usize) -> bool {
        self.alive[i]
    }

    /// Current text of token `i` (its replacement if it has one).
    pub(crate) fn text(&self, i: usize) -> &str {
        match &self.repl[i] {
            Some(s) => s,
            None => &self.src[self.toks[i].start..self.toks[i].end],
        }
    }

    pub(crate) fn is_word(&self, i: usize) -> bool {
        self.alive[i] && self.toks[i].kind == Kind::Word
    }

    pub(crate) fn delete(&mut self, i: usize) {
        if self.toks[i].kind == Kind::Protected {
            debug_assert!(false, "rules must never delete a protected span");
            return;
        }
        if self.alive[i] {
            self.alive[i] = false;
            self.changed = true;
        }
    }

    pub(crate) fn replace(&mut self, i: usize, text: String) {
        if self.toks[i].kind == Kind::Protected || text.chars().any(is_placeholder) {
            debug_assert!(false, "rules must never rewrite or forge a protected span");
            return;
        }
        if self.text(i) != text {
            self.repl[i] = Some(text);
            self.changed = true;
        }
    }

    /// Replace token `i` with text no later stage may alter.
    pub(crate) fn replace_protected(&mut self, i: usize, text: String, kind: SpanKind) {
        if self.toks[i].kind == Kind::Protected || self.spans.len() >= MAX_PROTECTED_SPANS {
            debug_assert!(self.toks[i].kind != Kind::Protected);
            return;
        }
        let p = char::from_u32(0xF_0000 + self.spans.len() as u32).expect("in range");
        self.spans.push(ProtectedSpan { kind, text });
        self.repl[i] = Some(p.to_string());
        self.changed = true;
    }

    pub(crate) fn next_alive(&self, i: usize) -> Option<usize> {
        (i + 1..self.toks.len()).find(|&j| self.alive[j])
    }

    pub(crate) fn prev_alive(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|&j| self.alive[j])
    }

    /// Next alive token that is not a space (may be a newline).
    pub(crate) fn next_solid(&self, i: usize) -> Option<usize> {
        (i + 1..self.toks.len()).find(|&j| self.alive[j] && self.toks[j].kind != Kind::Space)
    }

    /// Previous alive token that is not a space (may be a newline).
    pub(crate) fn prev_solid(&self, i: usize) -> Option<usize> {
        (0..i)
            .rev()
            .find(|&j| self.alive[j] && self.toks[j].kind != Kind::Space)
    }

    /// The alive token directly after `i` if it touches `i` (no space between).
    pub(crate) fn touching_next(&self, i: usize) -> Option<usize> {
        self.next_alive(i)
            .filter(|&j| self.toks[j].kind != Kind::Space)
    }

    /// The alive token directly before `i` if it touches `i`.
    pub(crate) fn touching_prev(&self, i: usize) -> Option<usize> {
        self.prev_alive(i)
            .filter(|&j| self.toks[j].kind != Kind::Space)
    }

    /// If `i` is followed by exactly one space token and then a word, that
    /// word's index and the space's index.
    pub(crate) fn next_word_after_space(&self, i: usize) -> Option<(usize, usize)> {
        let sp = self.next_alive(i)?;
        if self.toks[sp].kind != Kind::Space {
            return None;
        }
        let w = self.next_alive(sp)?;
        (self.toks[w].kind == Kind::Word).then_some((sp, w))
    }

    /// Whether token `i` begins a sentence: nothing before it (ignoring
    /// attached opening quotes/brackets), or a line break, or a `.`/`!`/`?`
    /// (optionally followed by closing quotes/brackets) and then a space.
    /// Abbreviations (`e.g.`, `vs.`, `a.m.`) and ellipses do not end one.
    pub(crate) fn at_sentence_start(&self, i: usize) -> bool {
        let mut k = i;
        while let Some(p) = self.touching_prev(k) {
            if self.toks[p].kind == Kind::Punct && is_opening(self.text(p)) {
                k = p;
            } else {
                break;
            }
        }
        let Some(p) = self.prev_solid(k) else {
            return true;
        };
        match self.toks[p].kind {
            Kind::Newline => true,
            Kind::Punct => {
                if !self
                    .prev_alive(k)
                    .is_some_and(|s| self.toks[s].kind == Kind::Space)
                {
                    return false;
                }
                let mut q = p;
                loop {
                    let t = self.text(q);
                    if is_terminal(t) {
                        return !is_abbreviation_before(self, q);
                    }
                    if !is_closing_quote(t) {
                        return false;
                    }
                    match self.touching_prev(q) {
                        Some(x) if self.toks[x].kind == Kind::Punct => q = x,
                        _ => return false,
                    }
                }
            }
            _ => false,
        }
    }

    /// Serialize alive tokens into `out`. Returns whether anything changed.
    pub(crate) fn finish(self, out: &mut String) -> bool {
        if !self.changed {
            return false;
        }
        out.clear();
        let mut pending: Option<usize> = None;
        let mut gap = false;
        let mut emitted = false;
        let mut last_kind: Option<Kind> = None;
        for i in 0..self.toks.len() {
            if !self.alive[i] {
                gap = true;
                continue;
            }
            let kind = self.toks[i].kind;
            let text = self.text(i);
            if kind == Kind::Space {
                if pending.is_none() {
                    pending = Some(i);
                }
                continue;
            }
            if let Some(p) = pending.take() {
                let debris = gap
                    && (!emitted
                        || last_kind == Some(Kind::Newline)
                        || kind == Kind::Newline
                        || text.starts_with('\n')
                        || is_closing(text));
                if !debris {
                    out.push_str(self.text(p));
                }
            }
            out.push_str(text);
            emitted = !text.is_empty() || emitted;
            last_kind = Some(if text.starts_with('\n') {
                Kind::Newline
            } else {
                kind
            });
            gap = false;
        }
        if let Some(p) = pending {
            if !gap {
                out.push_str(self.text(p));
            }
        }
        true
    }
}

/// Words that end in a period without ending the sentence.
const ABBREVIATIONS: &[&str] = &[
    "vs", "etc", "cf", "mr", "mrs", "ms", "dr", "prof", "jr", "sr", "st", "approx", "incl", "fig",
    "eg", "ie",
];

/// Whether the `.` at token `k` closes an abbreviation (`e.g.`, `vs.`, `a.m.`).
fn is_abbreviation_before(ed: &Editor<'_>, k: usize) -> bool {
    if ed.text(k) != "." {
        return false;
    }
    let Some(w) = ed.touching_prev(k) else {
        return false;
    };
    if ed.kind(w) != Kind::Word {
        return false;
    }
    let word = ed.text(w);
    if ABBREVIATIONS.iter().any(|a| a.eq_ignore_ascii_case(word)) {
        return true;
    }
    // Dotted single letters: e.g. / i.e. / a.m. / U.S.
    word.chars().count() == 1
        && ed.touching_prev(w).is_some_and(|d| {
            ed.text(d) == "."
                && ed
                    .touching_prev(d)
                    .is_some_and(|x| ed.kind(x) == Kind::Word && ed.text(x).chars().count() == 1)
        })
}

/// Uppercase the first character if it is a lowercase letter.
pub(crate) fn capitalized(word: &str) -> Option<String> {
    let mut chars = word.chars();
    let first = chars.next()?;
    if !first.is_lowercase() {
        return None;
    }
    let mut s = String::with_capacity(word.len() + 2);
    s.extend(first.to_uppercase());
    s.push_str(chars.as_str());
    Some(s)
}

pub(crate) fn starts_upper(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_uppercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(Kind, &str)> {
        lex(text)
            .into_iter()
            .map(|t| (t.kind, &text[t.start..t.end]))
            .collect()
    }

    #[test]
    fn words_keep_internal_apostrophes_hyphens_and_digit_separators() {
        use Kind::*;
        assert_eq!(
            kinds("don't twenty-five 3.14 5,000 wh- what"),
            vec![
                (Word, "don't"),
                (Space, " "),
                (Word, "twenty-five"),
                (Space, " "),
                (Word, "3.14"),
                (Space, " "),
                (Word, "5,000"),
                (Space, " "),
                (Word, "wh"),
                (Punct, "-"),
                (Space, " "),
                (Word, "what"),
            ]
        );
    }

    #[test]
    fn dots_run_together_and_sentence_periods_stay_separate() {
        use Kind::*;
        assert_eq!(
            kinds("wait... ok. 3."),
            vec![
                (Word, "wait"),
                (Punct, "..."),
                (Space, " "),
                (Word, "ok"),
                (Punct, "."),
                (Space, " "),
                (Word, "3"),
                (Punct, "."),
            ]
        );
    }

    #[test]
    fn deleting_a_word_leaves_no_spacing_debris() {
        let src = "we need um the config um.";
        let mut spans = Vec::new();
        let mut ed = Editor::new(src, &mut spans);
        for i in 0..ed.len() {
            if ed.text(i) == "um" {
                ed.delete(i);
            }
        }
        let mut out = String::new();
        assert!(ed.finish(&mut out));
        assert_eq!(out, "we need the config.");
    }

    #[test]
    fn an_untouched_editor_reports_no_change() {
        let mut spans = Vec::new();
        let ed = Editor::new("hello  ,  world", &mut spans);
        let mut out = String::new();
        assert!(!ed.finish(&mut out), "original spacing is spacing's job");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "never delete a protected span")]
    fn deleting_a_placeholder_is_a_bug() {
        let src = "a \u{F0000} b";
        let mut spans = vec![ProtectedSpan {
            kind: SpanKind::Code,
            text: "x_y".into(),
        }];
        let mut ed = Editor::new(src, &mut spans);
        ed.delete(2);
    }
}
