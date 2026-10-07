//! Suggestions only. This module issues SELECTs and never writes history or
//! dictionary entries, or emits transcript text to logs.
use chrono::{DateTime, Utc};
use dictate_proto::{DictionaryEntry, DictionarySuggestion, EntrySource};
use regex::Regex;
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

#[derive(Default)]
struct Evidence {
    count: u64,
    days: BTreeSet<String>,
    first: String,
    last: String,
}
impl Evidence {
    fn observe(&mut self, ts: &str) {
        self.count += 1;
        // The UTC day; a malformed timestamp counts as its own "day" rather
        // than panicking the miner.
        self.days.insert(ts.get(..10).unwrap_or(ts).into());
        if self.first.is_empty() || ts < self.first.as_str() {
            self.first = ts.into();
        }
        if self.last.is_empty() || ts > self.last.as_str() {
            self.last = ts.into();
        }
    }
    fn proposal(self, phrase: String, aliases: Vec<String>, reason: &str) -> DictionarySuggestion {
        let mut entry = DictionaryEntry::new(phrase);
        entry.sounds_like = aliases;
        entry.source = EntrySource::AutoLearned;
        DictionarySuggestion {
            entry,
            reason: reason.into(),
            count: self.count,
            days: self.days.len() as u32,
            first_seen: self.first,
            last_seen: self.last,
        }
    }
}
/// Open an existing history DB strictly read-only; a missing file is an error.
pub fn mine_path(
    path: impl AsRef<Path>,
    known: &[DictionaryEntry],
) -> anyhow::Result<Vec<DictionarySuggestion>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    mine(&conn, known)
}
/// Accepts the daemon's existing connection too, but performs only SELECTs.
pub fn mine(
    conn: &Connection,
    known: &[DictionaryEntry],
) -> anyhow::Result<Vec<DictionarySuggestion>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='interactions')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(Vec::new());
    }
    let known: HashSet<_> = known.iter().map(|e| crate::fold(&e.phrase)).collect();
    let word = Regex::new(r"[\p{L}\p{N}][\p{L}\p{M}\p{N}_]*")?;
    let mut rewrites: BTreeMap<(String, String), Evidence> = BTreeMap::new();
    let mut targets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut terms: BTreeMap<String, Evidence> = BTreeMap::new();
    let mut stmt=conn.prepare("SELECT timestamp, grammar_input, grammar_output, corrected_transcription, grammar_error FROM interactions WHERE completed=1 ORDER BY timestamp")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let timestamp: String = row.get(0)?;
        let Ok(parsed) = DateTime::parse_from_rfc3339(&timestamp) else {
            continue;
        };
        let timestamp = parsed.with_timezone(&Utc).to_rfc3339();
        let input: Option<String> = row.get(1)?;
        let output: Option<String> = row.get(2)?;
        let corrected: Option<String> = row.get(3)?;
        let error: Option<String> = row.get(4)?;
        if error.is_none() {
            if let (Some(a_text), Some(b_text)) = (&input, &output) {
                let a: Vec<_> = word.find_iter(a_text).collect();
                let b: Vec<_> = word.find_iter(b_text).collect();
                let same = |x: &regex::Match<'_>, y: &regex::Match<'_>| x.as_str() == y.as_str();
                let mut prefix = 0;
                while prefix < a.len().min(b.len()) && same(&a[prefix], &b[prefix]) {
                    prefix += 1;
                }
                let mut suffix = 0;
                while suffix < a.len().min(b.len()) - prefix
                    && same(&a[a.len() - 1 - suffix], &b[b.len() - 1 - suffix])
                {
                    suffix += 1;
                }
                let before = &a[prefix..a.len() - suffix];
                let after = &b[prefix..b.len() - suffix];
                // Ignore punctuation-only fixes and insertions/deletions. Larger
                // rewrites are prose edits, not evidence for a dictionary alias.
                if !before.is_empty() && !after.is_empty() && before.len() <= 4 && after.len() <= 4
                {
                    // The phrases are the original text spans, not the word
                    // tokens re-joined with spaces: "node jay ess" became
                    // `Node.js`, and joining its tokens would propose `Node js`
                    // (`miner-joins-tokens-corrupts-targets`).
                    let x = span(a_text, before);
                    let y = span(b_text, after);
                    if x != y
                        && x.chars().count() <= 200
                        && y.chars().count() <= 200
                        && !is_grammatical_rewrite(x, y)
                    {
                        targets
                            .entry(crate::fold(x))
                            .or_default()
                            .insert(y.to_string());
                        rewrites
                            .entry((crate::fold(x), y.to_string()))
                            .or_default()
                            .observe(&timestamp);
                    }
                }
            }
        }
        let text = if error.is_none() {
            output.as_ref().or(corrected.as_ref())
        } else {
            corrected.as_ref()
        };
        if let Some(text) = text {
            let mut last_word_end = 0;
            for m in word.find_iter(text) {
                let gap = &text[last_word_end..m.start()];
                let initial =
                    last_word_end == 0 || gap.chars().any(|c| matches!(c, '.' | '!' | '?' | '\n'));
                last_word_end = m.end();
                let token = m.as_str();
                if known.contains(&crate::fold(token)) {
                    continue;
                }
                let letters: Vec<_> = token.chars().filter(|c| c.is_alphabetic()).collect();
                if letters.len() < 2 {
                    continue;
                }
                let camel = letters.iter().skip(1).any(|c| c.is_uppercase())
                    && letters.iter().any(|c| c.is_lowercase());
                let acronym = letters.iter().all(|c| c.is_uppercase());
                let capital = letters[0].is_uppercase() && !initial;
                if camel || acronym || capital {
                    terms.entry(token.into()).or_default().observe(&timestamp);
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut proposed = HashSet::new();
    for ((x, y), e) in rewrites {
        if e.count >= 3
            && e.days.len() >= 2
            && !known.contains(&crate::fold(&y))
            && targets[&x].len() == 1
        {
            proposed.insert(crate::fold(&y));
            out.push(e.proposal(y, vec![x], "consistent_rewrite"));
        }
    }
    for (term, e) in terms {
        if e.count >= 5 && !proposed.contains(&crate::fold(&term)) && term.chars().count() <= 200 {
            out.push(e.proposal(term, vec![], "recurring_term"));
        }
    }
    out.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(a.entry.phrase.cmp(&b.entry.phrase))
    });
    Ok(out)
}

/// The original text from the first to the last of `words` (non-empty).
fn span<'t>(text: &'t str, words: &[regex::Match<'t>]) -> &'t str {
    &text[words[0].start()..words[words.len() - 1].end()]
}

/// Whether rewriting `source` → `target` is grammar, not vocabulary — and so
/// must never become a dictionary alias, which would rewrite every future
/// occurrence of `source` (`your` → `you're` would break every correct
/// "your").
///
/// Rejected: any rewrite of a single common English word, and any rewrite in
/// which every word on both sides is common (contractions such as
/// `do not` → `don't`, articles `a` → `an`, agreement `go` → `goes`, …). A
/// dictionary alias is for a term Whisper cannot spell — `kube net ease` →
/// `Kubernetes` — which by construction is not common English.
fn is_grammatical_rewrite(source: &str, target: &str) -> bool {
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let common = |w: &String| COMMON_WORDS.binary_search(&w.as_str()).is_ok();
    let (source, target) = (words(source), words(target));
    if source.len() == 1 && common(&source[0]) {
        return true;
    }
    source.iter().chain(&target).all(common)
}

/// Common English words, sorted (binary-searched), plus the fragments
/// contractions split into (`re`, `ll`, `ve`, `t`, `s`, `d`, `m`).
const COMMON_WORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "ago",
    "all",
    "almost",
    "also",
    "always",
    "am",
    "an",
    "and",
    "another",
    "any",
    "anyone",
    "anything",
    "are",
    "aren",
    "around",
    "as",
    "ask",
    "asked",
    "at",
    "away",
    "back",
    "bad",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "best",
    "better",
    "between",
    "big",
    "both",
    "but",
    "by",
    "call",
    "came",
    "can",
    "cannot",
    "case",
    "change",
    "come",
    "comes",
    "could",
    "couldn",
    "d",
    "day",
    "did",
    "didn",
    "different",
    "do",
    "does",
    "doesn",
    "doing",
    "don",
    "done",
    "down",
    "during",
    "each",
    "early",
    "either",
    "else",
    "end",
    "enough",
    "even",
    "ever",
    "every",
    "everything",
    "fact",
    "far",
    "few",
    "find",
    "first",
    "for",
    "found",
    "from",
    "get",
    "gets",
    "getting",
    "give",
    "go",
    "goes",
    "going",
    "gone",
    "good",
    "got",
    "great",
    "had",
    "hadn",
    "has",
    "hasn",
    "have",
    "haven",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "herself",
    "high",
    "him",
    "himself",
    "his",
    "how",
    "however",
    "i",
    "if",
    "in",
    "into",
    "is",
    "isn",
    "it",
    "its",
    "itself",
    "just",
    "keep",
    "kind",
    "know",
    "last",
    "later",
    "least",
    "less",
    "let",
    "lets",
    "like",
    "line",
    "little",
    "ll",
    "long",
    "look",
    "lot",
    "m",
    "made",
    "make",
    "makes",
    "making",
    "man",
    "many",
    "may",
    "maybe",
    "me",
    "mean",
    "might",
    "mine",
    "more",
    "most",
    "much",
    "must",
    "mustn",
    "my",
    "myself",
    "need",
    "needs",
    "never",
    "new",
    "next",
    "no",
    "nor",
    "not",
    "nothing",
    "now",
    "number",
    "of",
    "off",
    "often",
    "old",
    "on",
    "once",
    "one",
    "only",
    "or",
    "other",
    "others",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "part",
    "people",
    "place",
    "point",
    "put",
    "quite",
    "rather",
    "re",
    "really",
    "right",
    "s",
    "said",
    "same",
    "saw",
    "say",
    "says",
    "see",
    "seem",
    "seen",
    "set",
    "shall",
    "shan",
    "she",
    "should",
    "shouldn",
    "show",
    "since",
    "small",
    "so",
    "some",
    "someone",
    "something",
    "still",
    "such",
    "sure",
    "t",
    "take",
    "than",
    "thank",
    "thanks",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "things",
    "think",
    "this",
    "those",
    "though",
    "through",
    "time",
    "to",
    "too",
    "took",
    "try",
    "two",
    "under",
    "until",
    "up",
    "upon",
    "us",
    "use",
    "used",
    "using",
    "ve",
    "very",
    "want",
    "wants",
    "was",
    "wasn",
    "way",
    "we",
    "well",
    "went",
    "were",
    "weren",
    "what",
    "when",
    "where",
    "whether",
    "which",
    "while",
    "who",
    "whom",
    "whose",
    "why",
    "will",
    "with",
    "without",
    "won",
    "work",
    "works",
    "would",
    "wouldn",
    "y",
    "yeah",
    "year",
    "yes",
    "yet",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_common_word_list_is_sorted_for_binary_search() {
        assert!(COMMON_WORDS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn grammar_is_not_vocabulary() {
        for (a, b) in [
            ("your", "you're"),
            ("its", "it's"),
            ("do not", "don't"),
            ("a", "an"),
            ("go", "goes"),
            ("there", "their"),
            ("could of", "could have"),
        ] {
            assert!(is_grammatical_rewrite(a, b), "{a} -> {b}");
        }
        for (a, b) in [
            ("node jay ess", "Node.js"),
            ("kube net ease", "Kubernetes"),
            ("tow ree", "Tauri"),
        ] {
            assert!(!is_grammatical_rewrite(a, b), "{a} -> {b}");
        }
    }
}
