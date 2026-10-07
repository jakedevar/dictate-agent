use crate::{fold, DictionaryConfig, StoredEntry};
use aho_corasick::AhoCorasick;
use dictate_proto::AppContext;
use regex::Regex;
use std::{collections::HashSet, ops::Range};
use unicode_casefold::UnicodeCaseFold;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    /// Byte range in OUTPUT text; protect this before later formatting stages.
    pub range: Range<usize>,
    pub entry_id: i64,
    pub before: String,
    pub after: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub text: String,
    pub replacements: Vec<Replacement>,
}

pub(crate) struct Matcher {
    pub entries: Vec<StoredEntry>,
    aliases: Vec<(String, usize)>,
    automaton: AhoCorasick,
    word: Regex,
}
impl Matcher {
    pub fn new(mut entries: Vec<StoredEntry>, cfg: &DictionaryConfig) -> anyhow::Result<Self> {
        // Manual first, then popularity and edit recency, with a stable id tie-break.
        entries.sort_by_key(|e| {
            (
                e.entry.source != dictate_proto::EntrySource::Manual,
                std::cmp::Reverse(e.entry.hit_count.unwrap_or(0)),
                std::cmp::Reverse(e.updated_at),
                e.entry.id,
            )
        });
        let mut aliases = Vec::new();
        for (i, e) in entries.iter().enumerate().filter(|(_, e)| e.entry.enabled) {
            let mut seen = HashSet::new();
            for a in e.entry.sounds_like.iter().chain(
                (cfg.recase_phrases
                    && (e.entry.case_sensitive || !is_ordinary_phrase(&e.entry.phrase)))
                .then_some(&e.entry.phrase),
            ) {
                if seen.insert(if e.entry.case_sensitive {
                    a.clone()
                } else {
                    fold(a)
                }) {
                    aliases.push((a.clone(), i));
                }
            }
        }
        let automaton = AhoCorasick::new(aliases.iter().map(|(a, _)| fold(a)))?;
        Ok(Self {
            entries,
            aliases,
            automaton,
            word: Regex::new(r"\w")?,
        })
    }
    pub fn in_scope<'a>(
        &'a self,
        app: Option<&'a AppContext>,
    ) -> impl Iterator<Item = &'a StoredEntry> + 'a {
        self.entries.iter().filter(move |e| scoped(e, app))
    }
    pub fn apply(&self, text: &str, app: Option<&AppContext>, cfg: &DictionaryConfig) -> Applied {
        if !cfg.enabled || (!cfg.fuzzy && self.aliases.is_empty()) || text.is_empty() {
            return Applied {
                text: text.into(),
                replacements: Vec::new(),
            };
        }
        // Case folding can change UTF-8 width (İ -> i + combining dot). Map only
        // complete original character boundaries, never slice within an expansion.
        let mut folded = String::with_capacity(text.len());
        let mut offsets = vec![Some(0)];
        for (i, c) in text.char_indices() {
            let start = folded.len();
            folded.extend(c.to_lowercase().flat_map(|c| c.case_fold()));
            offsets.resize(folded.len() + 1, None);
            offsets[start] = Some(i);
            offsets[folded.len()] = Some(i + c.len_utf8());
        }
        let mut candidates = Vec::new();
        for m in self.automaton.find_overlapping_iter(&folded) {
            let (Some(start), Some(end)) = (offsets[m.start()], offsets[m.end()]) else {
                continue;
            };
            let (alias, index) = &self.aliases[m.pattern().as_usize()];
            let e = &self.entries[*index];
            if !scoped(e, app)
                || !self.boundary(text, start, end)
                || (e.entry.case_sensitive && &text[start..end] != alias)
            {
                continue;
            }
            // If multiple in-scope entries claim the same alias, decline rather
            // than silently choose a canonical spelling by database order.
            candidates.push((start, end, *index));
        }
        if cfg.fuzzy {
            for m in Regex::new(r"\w+").expect("constant regex").find_iter(text) {
                let token = m.as_str();
                if token.chars().count() < 6
                    || candidates
                        .iter()
                        .any(|(s, e, _)| *s < m.end() && *e > m.start())
                {
                    continue;
                }
                let folded_token = fold(token);
                // A canonical term already present must never be fuzzy-corrected.
                if self
                    .in_scope(app)
                    .any(|e| fold(&e.entry.phrase) == folded_token)
                {
                    continue;
                }
                let mut best = None;
                let mut score = cfg.fuzzy_threshold;
                let mut tied = false;
                for e in self.in_scope(app).filter(|e| {
                    !e.entry.case_sensitive && !e.entry.phrase.contains(char::is_whitespace)
                }) {
                    let candidate = fold(&e.entry.phrase);
                    let n = candidate.chars().count().max(folded_token.chars().count());
                    let distance = levenshtein(&folded_token, &candidate);
                    if distance > 2 {
                        continue;
                    }
                    let s = 1.0 - distance as f64 / n as f64;
                    if s > score || (s == score && best.is_none()) {
                        best = e.entry.id;
                        score = s;
                        tied = false;
                    } else if s == score {
                        tied = true;
                    }
                }
                if !tied {
                    if let Some(id) = best {
                        if let Some(i) = self.entries.iter().position(|e| e.entry.id == Some(id)) {
                            candidates.push((m.start(), m.end(), i));
                        }
                    }
                }
            }
        }
        let ambiguous: HashSet<(usize, usize)> = candidates
            .iter()
            .filter_map(|(s, e, i)| {
                candidates
                    .iter()
                    .any(|(s2, e2, j)| s == s2 && e == e2 && i != j)
                    .then_some((*s, *e))
            })
            .collect();
        candidates.retain(|(s, e, _)| !ambiguous.contains(&(*s, *e)));
        candidates.sort_unstable_by_key(|(s, e, i)| (std::cmp::Reverse(e - s), *s, *i));
        let mut selected = Vec::new();
        for c in candidates {
            if !selected.iter().any(|(s, e, _)| c.0 < *e && c.1 > *s) {
                selected.push(c);
            }
        }
        selected.sort_unstable_by_key(|(s, _, _)| *s);
        let mut out = String::with_capacity(text.len());
        let mut replacements = Vec::new();
        let mut cursor = 0;
        for (start, end, i) in selected {
            let e = &self.entries[i].entry;
            out.push_str(&text[cursor..start]);
            let output_start = out.len();
            out.push_str(&e.phrase);
            if text[start..end] != e.phrase {
                replacements.push(Replacement {
                    range: output_start..out.len(),
                    entry_id: e.id.expect("stored entry id"),
                    before: text[start..end].into(),
                    after: e.phrase.clone(),
                });
            }
            cursor = end;
        }
        out.push_str(&text[cursor..]);
        Applied {
            text: out,
            replacements,
        }
    }
    /// Whether `text[start..end]` is a whole word (or phrase) of its own.
    ///
    /// An apostrophe or hyphen *between word characters* is part of the word
    /// (`matcher-contraction-inside-word`): "don't", "can't", "won't" and
    /// "e-mail" are single words, so an entry `Don`, `Can` or `Mail` must not
    /// match inside them. The one exception is a possessive: a match may end
    /// before `'s` / `’s` that is itself followed by a non-word character,
    /// so "Tauri's" still recases.
    fn boundary(&self, text: &str, start: usize, end: usize) -> bool {
        let word = |c: char| self.word.is_match(c.encode_utf8(&mut [0; 4]));
        let joiner = |c: char| matches!(c, '\'' | '\u{2019}' | '-');
        let mut before = text[..start].chars().rev();
        match before.next() {
            Some(c) if word(c) => return false,
            Some(c) if joiner(c) && before.next().is_some_and(word) => return false,
            _ => {}
        }
        let mut after = text[end..].chars();
        match after.next() {
            Some(c) if word(c) => false,
            Some('\'' | '\u{2019}') => match after.next() {
                // A possessive `'s` ends the word; any other letter continues it.
                Some('s' | 'S') => !after.next().is_some_and(word),
                Some(c) => !word(c),
                None => true,
            },
            Some('-') => !after.next().is_some_and(word),
            _ => true,
        }
    }
}
/// Ordinary lowercase English words that are also names, products or
/// languages (`Rust`, `Swift`, `Apple`, `Mark`, `Will`, …). Sorted.
///
/// With `recase_phrases` on, an entry's own phrase is a match for its
/// lowercase spelling — `kubernetes` → `Kubernetes`. For an entry that is an
/// ordinary word that would recase every plain "rust" or "will" in running
/// prose, so such a phrase is **not** recased on its own: list the spoken
/// form in `sounds_like` (an explicit alias is always honoured) or mark the
/// entry `case_sensitive`. The list is deliberately short and conservative;
/// it holds words that dictation produces in lowercase prose all the time.
/// (`Don`, `Won` and `Can` stay recased: those are the user's own names, and
/// the contraction rule already keeps them out of "don't".)
const ORDINARY_WORDS: &[&str] = &[
    "apple", "april", "art", "bill", "bob", "bridge", "chat", "chrome", "dash", "drive", "edge",
    "elm", "flow", "gem", "go", "grace", "hope", "jack", "jade", "june", "kit", "lens", "light",
    "link", "march", "mark", "may", "mercury", "mint", "nest", "next", "note", "notion", "page",
    "pat", "pilot", "pine", "pipe", "pop", "post", "present", "pro", "rose", "ruby", "rust",
    "safari", "slack", "spark", "stripe", "swift", "teams", "will", "zoom",
];

/// Whether every word of `phrase` is an ordinary English word (see
/// [`ORDINARY_WORDS`]). A single distinctive word makes the phrase a term.
fn is_ordinary_phrase(phrase: &str) -> bool {
    let mut words = phrase
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .peekable();
    words.peek().is_some() && words.all(|w| ORDINARY_WORDS.binary_search(&fold(w).as_str()).is_ok())
}

fn scoped(e: &StoredEntry, app: Option<&AppContext>) -> bool {
    e.entry.enabled
        && (e.entry.apps.is_empty()
            || app.is_some_and(|app| e.entry.apps.iter().any(|a| fold(a) == fold(&app.app))))
}
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, c) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, d) in b.iter().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(prev + usize::from(c != *d));
            prev = old;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ordinary_word_list_is_sorted_for_binary_search() {
        assert!(ORDINARY_WORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(ORDINARY_WORDS.iter().all(|w| *w == w.to_lowercase()));
    }
}
