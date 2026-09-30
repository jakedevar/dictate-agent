//! `stutters`: immediate function-word repeats and cut-off fragments.
//!
//! Collapses "the the", "I I", "to to" — only for a closed list of articles,
//! pronouns, and conjunctions whose doubling is never grammatical — and
//! cut-off fragments: "wh- what", "st-stop", "I-I".
//!
//! Must-not-change: legitimate doubles ("that that", "had had", "is is",
//! "her her"), emphasis ("very very", "no no no", "so so", "my my"),
//! suspended hyphens ("pre- and post-processing"), and real hyphenated words
//! ("re-read", "T-test", "x-ray").

use crate::text::lex::{capitalized, starts_upper, Editor, Kind};
use crate::text::{FormatContext, TextDoc, TextStage};

/// Collapses stutters.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stutters;

impl TextStage for Stutters {
    fn name(&self) -> &'static str {
        "stutters"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        doc.edit(collapse);
    }
}

/// Words whose immediate repetition is a stutter, never grammar. Deliberately
/// excludes `that`, `had`, `is`, `her`, `you`, `me`, `my`, `he`, `so`, `no`,
/// and particles (`in`, `on`, `up`, …), which all double legitimately.
const REPEATABLE: &[&str] = &[
    "a", "an", "the", "i", "we", "our", "your", "his", "she", "they", "their", "them", "it", "its",
    "to", "of", "and", "but", "or", "with", "if", "this",
];

fn is_repeatable(w: &str) -> bool {
    REPEATABLE.iter().any(|r| r.eq_ignore_ascii_case(w))
}

fn has_vowel(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y'))
}

fn lower_starts_with(word: &str, prefix: &str) -> bool {
    word.len() >= prefix.len()
        && word.is_char_boundary(prefix.len())
        && word[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Carry a fragment's capital onto the word that replaces it.
fn with_case_of(fragment: &str, word: &str) -> String {
    if starts_upper(fragment) {
        capitalized(word).unwrap_or_else(|| word.to_string())
    } else {
        word.to_string()
    }
}

/// `st-stop` → `stop`, `I-I` → `I`. `None` for a real hyphenated word.
fn glued_stutter(word: &str) -> Option<String> {
    let (frag, rest) = word.split_once('-')?;
    if rest.contains('-') || frag.is_empty() || rest.is_empty() {
        return None;
    }
    if frag.eq_ignore_ascii_case("i") && rest.eq_ignore_ascii_case("i") {
        return Some("I".to_string());
    }
    // Two or three consonants cut off before the same word. One letter is
    // too often a real compound (T-test, X-ray, S-series); a vowel too often
    // a real prefix (re-read, co-op, pre-predict).
    let n = frag.chars().count();
    if (2..=3).contains(&n)
        && frag.chars().all(|c| c.is_ascii_alphabetic())
        && !has_vowel(frag)
        && rest.len() > frag.len()
        && rest.chars().all(char::is_alphabetic)
        && lower_starts_with(rest, frag)
    {
        return Some(with_case_of(frag, rest));
    }
    None
}

fn collapse(ed: &mut Editor<'_>) {
    let n = ed.len();
    let mut i = 0;
    while i < n {
        if !ed.is_word(i) {
            i += 1;
            continue;
        }
        if let Some(fixed) = glued_stutter(ed.text(i)) {
            ed.replace(i, fixed);
        }
        if let Some(next) = cut_off(ed, i) {
            i = next;
            continue;
        }
        repeats(ed, i);
        i += 1;
    }
}

/// "wh- what" → "what": a fragment, a dash, a space, and a word the fragment
/// begins. Returns the index to resume from.
fn cut_off(ed: &mut Editor<'_>, i: usize) -> Option<usize> {
    let dash = ed.touching_next(i)?;
    if ed.kind(dash) != Kind::Punct || !matches!(ed.text(dash), "-" | "\u{2013}" | "\u{2014}") {
        return None;
    }
    let (space, word) = ed.next_word_after_space(dash)?;
    let frag = ed.text(i);
    let next = ed.text(word);
    let n = frag.chars().count();
    if !(1..=4).contains(&n)
        || !frag.chars().all(char::is_alphabetic)
        || !lower_starts_with(next, frag)
    {
        return None;
    }
    let replacement = with_case_of(frag, next);
    ed.delete(i);
    ed.delete(dash);
    ed.delete(space);
    ed.replace(word, replacement);
    Some(word)
}

/// "the the the" → "the". Keeps the first occurrence (and its case).
fn repeats(ed: &mut Editor<'_>, i: usize) {
    let first = ed.text(i).to_string();
    if !is_repeatable(&first) {
        return;
    }
    let sentence_start = ed.at_sentence_start(i);
    while let Some((space, j)) = ed.next_word_after_space(i) {
        let other = ed.text(j);
        // Exact repeat, or a sentence-initial capital followed by the same
        // word in lowercase ("The the"). "Vitamin A a day" is not a stutter.
        let same =
            other == first || (sentence_start && capitalized(other).is_some_and(|c| c == first));
        if !same {
            break;
        }
        ed.delete(space);
        ed.delete(j);
    }
}

#[cfg(test)]
mod tests {
    use crate::text::rules::test_support::stage;

    use super::*;

    #[test]
    fn collapses_stutters() {
        let cases: &[(&str, &str)] = &[
            ("the the config is broken", "the config is broken"),
            ("The the config is broken", "The config is broken"),
            ("I I think so", "I think so"),
            ("we need to to go", "we need to go"),
            ("the the the end", "the end"),
            ("wh- what is this", "what is this"),
            ("Wh- what is this", "What is this"),
            ("I- I think", "I think"),
            ("please st-stop that", "please stop that"),
            ("I-I agree", "I agree"),
            ("it's b- but not now", "it's but not now"),
            ("and and then", "and then"),
        ];
        for (input, want) in cases {
            assert_eq!(stage(&Stutters, input), *want, "input: {input:?}");
        }
    }

    #[test]
    fn keeps_legitimate_doubles_and_emphasis() {
        for input in [
            "I know that that is true",
            "She had had enough",
            "What it is is a bug",
            "I gave her her keys",
            "It was very very slow",
            "No no no, not that",
            "It was so so",
            "My my, look at that",
            "Take Vitamin A a day",
            "pre- and post-processing",
            "first- and second-order terms",
            "please re-read it",
            "run a T-test",
            "an X-ray image",
            "the S-series",
            "a co-op model",
            "the, the thing",
            "the\nthe thing",
            "it is what it is",
        ] {
            assert_eq!(stage(&Stutters, input), input, "input: {input:?}");
        }
    }
}
