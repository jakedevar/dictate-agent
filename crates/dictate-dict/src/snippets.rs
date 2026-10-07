//! S24 snippets: a spoken trigger phrase expands to stored text.
//!
//! Storage lives in `dictionary.db` (migration 3) next to the dictionary and is
//! served through the same [`Dictionary`] handle. Matching is pure and
//! in-memory over an immutable snapshot, exactly like the dictionary's:
//!
//! * a trigger is matched on **words**, case-insensitively, so Whisper's
//!   capitalisation and the commas it sometimes drops between words
//!   (`"Insert, work email"`) do not defeat it;
//! * the longest trigger wins and matches never overlap;
//! * a trigger that ends the utterance swallows the sentence punctuation
//!   Whisper appended, so `"insert work email."` expands to the stored text
//!   alone;
//! * the stored text may contain `{date}`, `{time}`, `{clipboard}` and
//!   `{selection}` variables, resolved lazily through a [`SnippetVariables`]
//!   the caller supplies (`{{date}}` writes a literal `{date}`).
//!
//! This module never touches text it did not match; wiring the result into the
//! text chain as a protected span is `dictate-core::snippet_stage`.

use crate::{fold, store::internal, Dictionary, DictionaryStore};
use chrono::Utc;
use dictate_proto::{AppContext, ErrorCode, ProtoError, Snippet};
use regex::{Captures, Regex};
use rusqlite::{params, OptionalExtension};
use std::{
    collections::HashMap,
    ops::Range,
    sync::{Arc, OnceLock},
};

/// Variables a snippet expansion may reference, in `{name}` form.
pub const VARIABLES: [&str; 4] = ["date", "time", "clipboard", "selection"];

/// Longest expansion a snippet may store, in characters.
pub const MAX_EXPANSION_CHARS: usize = 4000;

/// Supplies the values of the expansion variables.
///
/// Called only for a variable the matched snippet actually uses, so reading the
/// clipboard costs nothing for a snippet that does not mention it.
pub trait SnippetVariables: Send + Sync {
    /// The value of `name` (one of [`VARIABLES`]), or `None` to leave the
    /// `{name}` placeholder in the text as written.
    fn resolve(&self, name: &str) -> Option<String>;
}

/// Resolves nothing: every `{variable}` stays literal.
pub struct NoVariables;
impl SnippetVariables for NoVariables {
    fn resolve(&self, _: &str) -> Option<String> {
        None
    }
}

/// One trigger found in a piece of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetMatch {
    /// Byte range of the *input* text the trigger (and any swallowed trailing
    /// sentence punctuation) occupies.
    pub range: Range<usize>,
    pub snippet_id: i64,
    /// The text that was spoken, as transcribed.
    pub spoken: String,
    /// The stored expansion template, variables unresolved.
    pub template: String,
}

/// Replace `{variable}` placeholders in `template` in a single pass: a resolved
/// value is never scanned again, so clipboard text containing `{selection}` is
/// inserted as it is.
#[must_use]
pub fn expand(template: &str, vars: &dyn SnippetVariables) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\{\{(\w+)\}\}|\{(\w+)\}").expect("static regex"));
    re.replace_all(template, |caps: &Captures| {
        let whole = caps[0].to_string();
        if let Some(name) = caps.get(1).map(|m| m.as_str()) {
            // `{{name}}` escapes a real variable; anything else is left alone.
            return if VARIABLES.contains(&name) {
                format!("{{{name}}}")
            } else {
                whole
            };
        }
        let name = &caps[2];
        if !VARIABLES.contains(&name) {
            return whole;
        }
        vars.resolve(name).unwrap_or(whole)
    })
    .into_owned()
}

struct Token {
    range: Range<usize>,
    key: String,
}

fn tokens(text: &str) -> Vec<Token> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE
        .get_or_init(|| Regex::new(r"[\p{L}\p{N}]+(?:['’][\p{L}\p{N}]+)*").expect("static regex"));
    re.find_iter(text)
        .map(|m| Token {
            range: m.range(),
            key: fold(m.as_str()),
        })
        .collect()
}

/// What may sit between two spoken words of a trigger without breaking it.
fn gap_ok(gap: &str) -> bool {
    gap.chars().all(|c| {
        c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '-' | '–' | '—' | '…')
    })
}

/// The normalised form uniqueness is judged on: folded words joined by a space.
fn trigger_key(trigger: &str) -> String {
    tokens(trigger)
        .into_iter()
        .map(|t| t.key)
        .collect::<Vec<_>>()
        .join(" ")
}

fn valid_text(s: &str) -> bool {
    !s.trim().is_empty()
        && s == s.trim()
        && s.chars().count() <= 200
        && !s.chars().any(char::is_control)
}

/// Validate a snippet for storage. Pure; also used by the store.
pub fn validate(s: &Snippet) -> Result<(), ProtoError> {
    if !valid_text(&s.trigger) {
        return Err(invalid(
            "trigger must contain 1..200 characters, no control characters or outer whitespace",
        ));
    }
    // Every character must be part of a word or harmless separation: a trigger
    // is *spoken*, so symbols could never be matched against a transcript.
    let toks = tokens(&s.trigger);
    if toks.is_empty() {
        return Err(invalid("trigger must contain at least one word"));
    }
    let mut rest = String::new();
    let mut last = 0;
    for t in &toks {
        rest.push_str(&s.trigger[last..t.range.start]);
        last = t.range.end;
    }
    rest.push_str(&s.trigger[last..]);
    if !gap_ok(&rest) {
        return Err(invalid(
            "trigger may only contain words separated by spaces or simple punctuation",
        ));
    }
    if s.expansion.is_empty()
        || s.expansion.chars().count() > MAX_EXPANSION_CHARS
        || s.expansion
            .chars()
            .any(|c| (c.is_control() && c != '\n' && c != '\t') || is_private_use(c))
    {
        return Err(invalid(&format!(
            "expansion must be 1..{MAX_EXPANSION_CHARS} characters; only newline and tab \
             control characters and no private-use characters"
        )));
    }
    if s.id.is_some_and(|id| id <= 0) {
        return Err(invalid("snippet id must be positive"));
    }
    if s.apps.len() > 64 || s.apps.iter().any(|a| !valid_text(a)) {
        return Err(invalid(
            "apps must contain at most 64 nonempty strings of 1..200 characters",
        ));
    }
    if s.category.as_deref().is_some_and(|c| !valid_text(c)) {
        return Err(invalid("category must be 1..200 characters when present"));
    }
    Ok(())
}

/// The text chain reserves Unicode private-use planes for protected-span
/// placeholders, so stored text must not contain any.
fn is_private_use(c: char) -> bool {
    matches!(c as u32, 0xE000..=0xF8FF | 0xF_0000..=0xF_FFFD | 0x10_0000..=0x10_FFFD)
}

fn invalid(s: &str) -> ProtoError {
    ProtoError::new(ErrorCode::InvalidParams, s)
}

/// An immutable, indexed snapshot of the stored snippets.
pub(crate) struct SnippetMatcher {
    pub snippets: Vec<Snippet>,
    /// Folded trigger words per snippet (parallel to `snippets`).
    keys: Vec<Vec<String>>,
    /// First trigger word → indices of the snippets that start with it.
    by_first: HashMap<String, Vec<usize>>,
}

impl SnippetMatcher {
    pub fn new(snippets: Vec<Snippet>) -> Self {
        let keys: Vec<Vec<String>> = snippets
            .iter()
            .map(|s| tokens(&s.trigger).into_iter().map(|t| t.key).collect())
            .collect();
        let mut by_first: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, s) in snippets.iter().enumerate() {
            if s.enabled {
                if let Some(first) = keys[i].first() {
                    by_first.entry(first.clone()).or_default().push(i);
                }
            }
        }
        Self {
            snippets,
            keys,
            by_first,
        }
    }

    fn scoped(&self, i: usize, app: Option<&AppContext>) -> bool {
        let s = &self.snippets[i];
        s.enabled
            && (s.apps.is_empty()
                || app.is_some_and(|app| s.apps.iter().any(|a| fold(a) == fold(&app.app))))
    }

    pub fn find(&self, text: &str, app: Option<&AppContext>) -> Vec<SnippetMatch> {
        if self.by_first.is_empty() {
            return Vec::new();
        }
        let toks = tokens(text);
        // (first token, token count, snippet index)
        let mut candidates: Vec<(usize, usize, usize)> = Vec::new();
        for (i, t) in toks.iter().enumerate() {
            for &s in self.by_first.get(&t.key).into_iter().flatten() {
                let k = &self.keys[s];
                if i + k.len() > toks.len() || !self.scoped(s, app) {
                    continue;
                }
                let fits = (1..k.len()).all(|j| {
                    toks[i + j].key == k[j]
                        && gap_ok(&text[toks[i + j - 1].range.end..toks[i + j].range.start])
                });
                if fits {
                    candidates.push((i, k.len(), s));
                }
            }
        }
        // Leftmost first; at one position the longest trigger wins.
        candidates.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let mut out = Vec::new();
        let mut free_from = 0;
        for (i, n, s) in candidates {
            if i < free_from {
                continue;
            }
            free_from = i + n;
            let start = toks[i].range.start;
            let end = toks[i + n - 1].range.end;
            out.push((start, end, s));
        }
        // A trigger that ends the utterance swallows the sentence punctuation
        // Whisper appended to it.
        let mut matches: Vec<SnippetMatch> = out
            .into_iter()
            .map(|(start, end, s)| SnippetMatch {
                range: start..end,
                snippet_id: self.snippets[s].id.unwrap_or_default(),
                spoken: text[start..end].to_string(),
                template: self.snippets[s].expansion.clone(),
            })
            .collect();
        if let Some(last) = matches.last_mut() {
            let rest = &text[last.range.end..];
            if rest
                .chars()
                .all(|c| c.is_whitespace() || is_sentence_punctuation(c))
            {
                let swallowed: usize = rest
                    .chars()
                    .take_while(|&c| is_sentence_punctuation(c))
                    .map(char::len_utf8)
                    .sum();
                last.range.end += swallowed;
            }
        }
        matches
    }
}

fn is_sentence_punctuation(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | ',' | ';' | ':' | '…')
}

// --- storage -----------------------------------------------------------------

impl DictionaryStore {
    pub fn snippets(&self) -> anyhow::Result<Vec<Snippet>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, trigger_text, expansion, category, apps, enabled, hit_count \
             FROM snippets ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
                r.get::<_, u64>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (id, trigger, expansion, category, apps, enabled, hits) = row?;
            Ok(Snippet {
                id: Some(id),
                trigger,
                expansion,
                enabled,
                category,
                apps: serde_json::from_str(&apps)?,
                hit_count: Some(hits),
            })
        })
        .collect()
    }

    pub fn upsert_snippet(&mut self, mut snippet: Snippet) -> Result<Snippet, ProtoError> {
        validate(&snippet)?;
        let now = Utc::now().timestamp_millis();
        let key = trigger_key(&snippet.trigger);
        let apps = serde_json::to_string(&snippet.apps).map_err(internal)?;
        let tx = self.conn.transaction().map_err(internal)?;
        let conflicting: Option<i64> = tx
            .query_row(
                "SELECT id FROM snippets WHERE trigger_key=?1",
                [&key],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        if conflicting.is_some() && conflicting != snippet.id {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "a snippet with this trigger already exists",
            ));
        }
        if let Some(id) = snippet.id {
            let changed = tx
                .execute(
                    "UPDATE snippets SET trigger_text=?1,trigger_key=?2,expansion=?3,category=?4,\
                     apps=?5,enabled=?6,updated_at=?7 WHERE id=?8",
                    params![
                        snippet.trigger,
                        key,
                        snippet.expansion,
                        snippet.category,
                        apps,
                        snippet.enabled,
                        now,
                        id
                    ],
                )
                .map_err(internal)?;
            if changed == 0 {
                return Err(ProtoError::new(
                    ErrorCode::NotFound,
                    "snippet does not exist",
                ));
            }
        } else {
            tx.execute(
                "INSERT INTO snippets (trigger_text,trigger_key,expansion,category,apps,enabled,\
                 created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
                params![
                    snippet.trigger,
                    key,
                    snippet.expansion,
                    snippet.category,
                    apps,
                    snippet.enabled,
                    now
                ],
            )
            .map_err(internal)?;
            snippet.id = Some(tx.last_insert_rowid());
        }
        // Hit counts are server-owned, including on update.
        snippet.hit_count = Some(
            tx.query_row(
                "SELECT hit_count FROM snippets WHERE id=?1",
                [snippet.id],
                |r| r.get(0),
            )
            .map_err(internal)?,
        );
        tx.commit().map_err(internal)?;
        Ok(snippet)
    }

    pub fn delete_snippet(&mut self, id: i64) -> Result<(), ProtoError> {
        if id <= 0 {
            return Err(invalid("snippet id must be positive"));
        }
        if self
            .conn
            .execute("DELETE FROM snippets WHERE id=?1", [id])
            .map_err(internal)?
            == 0
        {
            return Err(ProtoError::new(
                ErrorCode::NotFound,
                "snippet does not exist",
            ));
        }
        Ok(())
    }

    pub fn increment_snippet_hits(&mut self, hits: &HashMap<i64, u64>) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        for (id, count) in hits {
            tx.execute(
                "UPDATE snippets SET hit_count=hit_count+?1 WHERE id=?2",
                params![count, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

// --- the shared handle ---------------------------------------------------------

impl Dictionary {
    /// Stored snippets in creation order, optionally filtered by a substring of
    /// the trigger.
    pub fn list_snippets(&self, query: Option<&str>, limit: Option<u32>) -> Vec<Snippet> {
        let snapshot = self.snippets.read().expect("snippet snapshot poisoned");
        let q = query.map(fold);
        snapshot
            .snippets
            .iter()
            .filter(|s| q.as_ref().is_none_or(|q| fold(&s.trigger).contains(q)))
            .take(limit.map(|n| n.min(10_000) as usize).unwrap_or(usize::MAX))
            .cloned()
            .collect()
    }

    pub fn upsert_snippet(&self, snippet: Snippet) -> Result<Snippet, ProtoError> {
        let mut store = self.store.lock().map_err(internal)?;
        let snippet = store.upsert_snippet(snippet)?;
        self.refresh_snippets(&store).map_err(internal)?;
        Ok(snippet)
    }

    pub fn delete_snippet(&self, id: i64) -> Result<(), ProtoError> {
        let mut store = self.store.lock().map_err(internal)?;
        store.delete_snippet(id)?;
        self.refresh_snippets(&store).map_err(internal)?;
        Ok(())
    }

    pub(crate) fn refresh_snippets(&self, store: &DictionaryStore) -> anyhow::Result<()> {
        let next = Arc::new(SnippetMatcher::new(store.snippets()?));
        *self
            .snippets
            .write()
            .map_err(|_| anyhow::anyhow!("snippet snapshot poisoned"))? = next;
        Ok(())
    }

    /// Pure matching; call [`Dictionary::record_snippet_hits`] separately, and
    /// only for sessions that may leave a trace. Empty when the dictionary is
    /// disabled in config.
    pub fn find_snippets(&self, text: &str, app: Option<&AppContext>) -> Vec<SnippetMatch> {
        if !self.config.enabled {
            return Vec::new();
        }
        let snapshot = self
            .snippets
            .read()
            .expect("snippet snapshot poisoned")
            .clone();
        snapshot.find(text, app)
    }

    pub fn record_snippet_hits(&self, matches: &[SnippetMatch], privacy: bool) {
        if privacy {
            return;
        }
        let mut pending = self
            .snippet_pending
            .lock()
            .expect("snippet hit queue poisoned");
        for m in matches {
            *pending.entry(m.snippet_id).or_default() += 1;
        }
    }

    /// Persist queued snippet hits. Failed batches stay queued.
    pub(crate) fn flush_snippet_hits(&self, store: &mut DictionaryStore) -> anyhow::Result<()> {
        let batch = std::mem::take(
            &mut *self
                .snippet_pending
                .lock()
                .map_err(|_| anyhow::anyhow!("snippet hit queue poisoned"))?,
        );
        if batch.is_empty() {
            return Ok(());
        }
        if let Err(e) = store.increment_snippet_hits(&batch) {
            let mut pending = self
                .snippet_pending
                .lock()
                .map_err(|_| anyhow::anyhow!("snippet hit queue poisoned"))?;
            for (id, count) in batch {
                *pending.entry(id).or_default() += count;
            }
            return Err(e);
        }
        self.refresh_snippets(store)
    }
}
