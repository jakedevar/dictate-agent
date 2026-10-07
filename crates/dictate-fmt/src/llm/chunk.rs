//! Splitting long inputs at paragraph and sentence boundaries.
//!
//! Each chunk is formatted by its own request, so a chunk boundary must be a
//! place where the text can be cut without changing how either side reads:
//! after a sentence or paragraph, never inside a protected span. Chunks tile
//! the input exactly — concatenating them reproduces it byte for byte — so a
//! chunk whose output is rejected can fall back to its own input without
//! disturbing its neighbours.

use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;

/// Byte positions where a new chunk may start: just after a sentence end
/// (`.`, `!`, `?`, optionally followed by closing quotes/brackets) and its
/// whitespace, or after a blank line.
fn boundaries(text: &str, spans: &[Range<usize>]) -> Vec<usize> {
    static SENTENCE: OnceLock<Regex> = OnceLock::new();
    let re = SENTENCE.get_or_init(|| Regex::new(r#"[.!?]["'”’)\]]*\s+|\n[ \t]*\n\s*"#).unwrap());
    re.find_iter(text)
        .map(|m| m.end())
        .filter(|&end| end < text.len())
        .filter(|&end| !spans.iter().any(|s| s.start < end && end < s.end))
        .collect()
}

/// Split `text` into chunks of about `chunk_words` words. A single sentence
/// longer than that stays whole: cutting mid-sentence would make the model
/// punctuate a fragment as if it were complete.
#[must_use]
pub fn split(text: &str, spans: &[Range<usize>], chunk_words: usize) -> Vec<Range<usize>> {
    let chunk_words = chunk_words.max(1);
    let mut cuts = boundaries(text, spans);
    cuts.push(text.len());

    let mut chunks = Vec::new();
    let mut start = 0;
    let mut words = 0;
    let mut prev = 0;
    for cut in cuts {
        let sentence_words = text[prev..cut].split_whitespace().count();
        if words > 0 && words + sentence_words > chunk_words {
            chunks.push(start..prev);
            start = prev;
            words = 0;
        }
        words += sentence_words;
        prev = cut;
    }
    if start < text.len() || chunks.is_empty() {
        chunks.push(start..text.len());
    }
    chunks
}

/// `(leading whitespace, core, trailing whitespace)` of a chunk, so the
/// model sees only the core and the original spacing is kept.
#[must_use]
pub fn trim_parts(s: &str) -> (&str, &str, &str) {
    let core_start = s.len() - s.trim_start().len();
    let core_end = s.trim_end().len().max(core_start);
    (&s[..core_start], &s[core_start..core_end], &s[core_end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces<'a>(text: &'a str, spans: &[Range<usize>], n: usize) -> Vec<&'a str> {
        split(text, spans, n)
            .into_iter()
            .map(|r| &text[r])
            .collect()
    }

    #[test]
    fn chunks_tile_the_input_exactly() {
        let text = "One two three. Four five six! Seven eight? Nine ten.\n\nEleven twelve.";
        for n in 1..8 {
            let joined: String = pieces(text, &[], n).concat();
            assert_eq!(joined, text, "chunk_words={n}");
        }
    }

    #[test]
    fn packs_sentences_up_to_the_target() {
        let text = "One two three. Four five six. Seven eight nine. Ten.";
        assert_eq!(
            pieces(text, &[], 6),
            ["One two three. Four five six. ", "Seven eight nine. Ten."]
        );
    }

    #[test]
    fn a_long_sentence_is_never_cut() {
        let text = "a b c d e f g h i j. k l.";
        assert_eq!(pieces(text, &[], 3), ["a b c d e f g h i j. ", "k l."]);
    }

    #[test]
    fn paragraphs_are_boundaries() {
        let text = "first para words here\n\nsecond para words here";
        assert_eq!(
            pieces(text, &[], 4),
            ["first para words here\n\n", "second para words here"]
        );
    }

    #[test]
    fn never_splits_inside_a_protected_span() {
        // A span that itself contains ". " (backtick code) is not a boundary.
        let text = "Run `a. b` now please. Then stop.";
        let span = text.find('`').unwrap()..text.rfind('`').unwrap() + 1;
        let chunks = split(text, std::slice::from_ref(&span), 1);
        for c in &chunks {
            assert!(!(c.start > span.start && c.start < span.end), "{chunks:?}");
        }
        assert_eq!(
            pieces(text, &[span], 1),
            ["Run `a. b` now please. ", "Then stop."]
        );
    }

    #[test]
    fn unpunctuated_text_is_one_chunk() {
        let text = "no punctuation at all just words";
        assert_eq!(pieces(text, &[], 2), [text]);
        assert_eq!(pieces("", &[], 2), [""]);
    }

    #[test]
    fn trim_parts_keeps_spacing() {
        assert_eq!(trim_parts("  hi there \n"), ("  ", "hi there", " \n"));
        assert_eq!(trim_parts("   "), ("   ", "", ""));
    }
}
