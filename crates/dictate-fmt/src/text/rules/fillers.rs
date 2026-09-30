//! `fillers`: standalone disfluencies, with punctuation and casing repaired.
//!
//! Only sounds that are never words: um, uh, erm, er, hmm, mm, mhm (and their
//! elongations — umm, uhhh, hmmm). Discourse words that *can* be filler
//! ("like", "you know", "I mean", "so", "basically", "actually") carry meaning
//! often enough that removing them needs context; that is S21's job.
//!
//! Must-not-change: fillers inside words (umbrella, Uhura), all-caps
//! acronyms (ER, UM), `err` (a verb), affirmatives (mm-hmm, uh-huh, uh-oh),
//! and `mm` after a number (millimetres).

use crate::text::lex::{capitalized, is_terminal, starts_upper, Editor, Kind};
use crate::text::{FormatContext, TextDoc, TextStage};

use super::numbers::is_number_word;

/// Removes standalone disfluencies.
#[derive(Debug, Default, Clone, Copy)]
pub struct Fillers;

impl TextStage for Fillers {
    fn name(&self) -> &'static str {
        "fillers"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        doc.edit(remove_fillers);
    }
}

/// Whether `word` is one of the filler shapes, by run-length letter pattern.
pub(crate) fn is_filler(word: &str) -> bool {
    if word.len() > 12 || !word.is_ascii() {
        return false;
    }
    if word.len() >= 2 && word.chars().all(|c| c.is_ascii_uppercase()) {
        return false; // ER, UM, MM: acronyms, not sounds
    }
    let mut runs: [(u8, usize); 4] = [(0, 0); 4];
    let mut n = 0;
    for b in word.bytes().map(|b| b.to_ascii_lowercase()) {
        if n > 0 && runs[n - 1].0 == b {
            runs[n - 1].1 += 1;
        } else {
            if n == runs.len() {
                return false;
            }
            runs[n] = (b, 1);
            n += 1;
        }
    }
    let letters: Vec<u8> = runs[..n].iter().map(|r| r.0).collect();
    match letters.as_slice() {
        b"um" | b"uh" | b"uhm" | b"erm" | b"hm" | b"mhm" => true,
        // `er` exactly: `err` is a word.
        b"er" => runs[0].1 == 1 && runs[1].1 == 1,
        b"m" => runs[0].1 >= 2,
        _ => false,
    }
}

fn is_comma_like(s: &str) -> bool {
    matches!(
        s,
        "," | ";" | "..." | "\u{2026}" | "-" | "\u{2013}" | "\u{2014}"
    )
}

fn remove_fillers(ed: &mut Editor<'_>) {
    for i in 0..ed.len() {
        if !ed.is_word(i) || !is_filler(ed.text(i)) {
            continue;
        }
        let left = ed.prev_solid(i);
        // `5 mm`, `five mm`: a unit, not a sound.
        if ed.text(i).eq_ignore_ascii_case("mm")
            && left.is_some_and(|l| {
                ed.kind(l) == Kind::Word
                    && (ed
                        .text(l)
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
                        || is_number_word(ed.text(l)))
            })
        {
            continue;
        }
        let sentence_start = ed.at_sentence_start(i);
        let capital = starts_upper(ed.text(i));
        let left_comma = left.filter(|&l| ed.kind(l) == Kind::Punct && is_comma_like(ed.text(l)));
        let right = ed.touching_next(i).filter(|&r| ed.kind(r) == Kind::Punct);

        ed.delete(i);
        match right.map(|r| (r, ed.text(r).to_string())) {
            Some((r, p)) if is_comma_like(&p) => {
                // "Um, so" / "think, um, we": the filler's own comma goes.
                // "need um, the": keep it — it may be separating list items.
                if sentence_start || left_comma.is_some() {
                    ed.delete(r);
                }
            }
            Some((r, p)) if is_terminal(&p) => {
                if sentence_start {
                    ed.delete(r); // "Okay. Um. Let's" → "Okay. Let's"
                } else if let Some(l) = left_comma {
                    ed.delete(l); // "the config, um." → "the config."
                }
            }
            Some(_) => {}
            None => {
                // "the config, um" at the very end.
                if ed.next_solid(i).is_none() {
                    if let Some(l) = left_comma {
                        ed.delete(l);
                    }
                }
            }
        }
        if sentence_start && capital {
            if let Some(next) = ed.next_solid(i).filter(|&j| ed.kind(j) == Kind::Word) {
                if let Some(up) = capitalized(ed.text(next)) {
                    ed.replace(next, up);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::text::rules::test_support::stage;

    use super::*;

    #[test]
    fn removes_fillers_and_repairs_punctuation_and_casing() {
        let cases: &[(&str, &str)] = &[
            ("Um, so we need to fix it.", "So we need to fix it."),
            ("Uh so we need to fix it", "So we need to fix it"),
            ("I think, um, we should ship.", "I think, we should ship."),
            ("I think um we should ship.", "I think we should ship."),
            ("Check the config, um.", "Check the config."),
            ("Check the config um.", "Check the config."),
            ("Check the config, uh", "Check the config"),
            ("Okay. Um. Let's go.", "Okay. Let's go."),
            ("Hmm, interesting.", "Interesting."),
            ("Um, uh, so yes.", "So yes."),
            ("Ummm... well, fine.", "Well, fine."),
            ("We need, erm, three.", "We need, three."),
            ("Er, what was it?", "What was it?"),
            ("Is it, uh?", "Is it?"),
            ("Mhm, right.", "Right."),
            ("Um.", ""),
            ("line one\num next", "line one\nnext"),
            ("so uhh, yeah", "so, yeah"),
        ];
        for (input, want) in cases {
            assert_eq!(stage(&Fillers, input), *want, "input: {input:?}");
        }
    }

    #[test]
    fn never_touches_words_that_merely_look_like_fillers() {
        for input in [
            "Grab an umbrella.",
            "Uhura was on the bridge.",
            "It could be better.",
            "To err is human.",
            "The ER was busy.",
            "The UM campus.",
            "Mm-hmm, that works.",
            "Uh-huh.",
            "Uh-oh, it broke.",
            "Use a 5 mm drill bit.",
            "Use a five mm drill bit.",
            "Summer umpire humming.",
            "I like it, you know, basically.",
            "So, actually, I mean it.",
        ] {
            assert_eq!(stage(&Fillers, input), input, "input: {input:?}");
        }
    }

    #[test]
    fn filler_shapes() {
        for w in [
            "um", "Umm", "uh", "uhh", "uhm", "erm", "er", "hm", "hmm", "mm", "mmm", "mhm",
        ] {
            assert!(is_filler(w), "{w}");
        }
        for w in [
            "m", "err", "ER", "UM", "umbrella", "u", "h", "e", "uhum", "mmh",
        ] {
            assert!(!is_filler(w), "{w}");
        }
    }
}
