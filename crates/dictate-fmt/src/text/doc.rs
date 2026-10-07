//! The document every text stage edits: working text plus protected spans.
//!
//! # How protection works
//!
//! A protected span is cut out of the working text and replaced by a single
//! **placeholder character** from Unicode's Supplementary Private Use Area-A
//! (`U+F0000 + index`). No rule matches private-use characters — they are not
//! letters, digits, whitespace or punctuation — so a span is opaque to every
//! later stage without that stage having to know it exists. The token editor
//! the built-in rules use goes further and *refuses* to delete or rewrite a
//! placeholder, so "no rule can alter a protected span" holds by construction,
//! not by each rule's good behavior.
//!
//! [`TextDoc::restore`] swaps every placeholder back for its original bytes.
//! [`TextDoc::verify_output`] is the check an LLM pass must pass: every
//! protected span present, byte-identical, not duplicated, in order.
//!
//! # When protection does not fit
//!
//! The placeholder range holds [`MAX_PROTECTED_SPANS`] spans. A document that
//! needs more — a hostile or absurd input, never a dictation — is **raw**
//! ([`TextDoc::is_raw`]): it is left exactly as it was when protection
//! failed, every later edit is refused, and only byte-identical formatter
//! output verifies. Half-protecting it instead would let a later stage edit
//! text that should have been protected, or alias a placeholder.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use aho_corasick::{AhoCorasick, Input, MatchKind};

use super::lex::Editor;

const SENTINEL_BASE: u32 = 0xF_0000;
const SENTINEL_LAST: u32 = 0xF_FFFD;

/// The most spans one document can hold (one placeholder per code point in
/// Supplementary Private Use Area-A). A 1,500-word dictation uses a handful.
pub const MAX_PROTECTED_SPANS: usize = (SENTINEL_LAST - SENTINEL_BASE + 1) as usize;

/// Whether `c` is a placeholder character.
#[inline]
#[must_use]
pub fn is_placeholder(c: char) -> bool {
    (SENTINEL_BASE..=SENTINEL_LAST).contains(&(c as u32))
}

#[inline]
pub(crate) fn placeholder_for(index: usize) -> char {
    debug_assert!(index < MAX_PROTECTED_SPANS);
    char::from_u32(SENTINEL_BASE + index as u32).expect("placeholder index in range")
}

#[inline]
fn placeholder_index(c: char) -> Option<usize> {
    is_placeholder(c).then(|| (c as u32 - SENTINEL_BASE) as usize)
}

/// What a protected span is. Informational: every kind is equally opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SpanKind {
    /// `https://…`, `www.…`, or a bare `host.tld`.
    Url,
    /// `name@host.tld`.
    Email,
    /// `~/…`, `./…`, `../…`, `/abs/path`, `.dotdir/…`, `.dotfile`, `dir/file.ext`.
    Path,
    /// `/command_name` at a token start (Claude Code / Codex slash commands).
    SlashCommand,
    /// snake_case, camelCase, `dotted.calls()`, `a::b`, `--flag`, `$VAR`,
    /// `file.ext`, and anything in backticks.
    Code,
    /// `@mention` or `#channel`.
    Mention,
    /// A canonical dictionary term (S22).
    Term,
    /// A snippet placeholder (S24).
    Snippet,
    /// A private-use character already present in the input. Protected so it
    /// can never be confused with a placeholder and is restored byte-for-byte.
    Literal,
}

impl SpanKind {
    /// Stable lowercase name, for logs and error messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Email => "email",
            Self::Path => "path",
            Self::SlashCommand => "slash_command",
            Self::Code => "code",
            Self::Mention => "mention",
            Self::Term => "term",
            Self::Snippet => "snippet",
            Self::Literal => "literal",
        }
    }
}

/// One protected span: its kind and its exact original bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedSpan {
    /// What was protected.
    pub kind: SpanKind,
    /// The bytes that [`TextDoc::restore`] puts back.
    pub text: String,
}

/// A replacement over a byte range of the working text, for stages that find
/// their matches with their own matcher (S22's dictionary, S24's snippets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Byte range of [`TextDoc::working_text`] to replace.
    pub range: Range<usize>,
    /// What goes there.
    pub replacement: Replacement,
}

/// What an [`Edit`] inserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replacement {
    /// Ordinary text; later stages may still edit it.
    Text(String),
    /// Text no later stage may alter — the "protect what you produce" API.
    Protected {
        /// The bytes to protect.
        text: String,
        /// Why.
        kind: SpanKind,
    },
}

impl Edit {
    /// Replace `range` with ordinary text.
    #[must_use]
    pub fn text(range: Range<usize>, text: impl Into<String>) -> Self {
        Self {
            range,
            replacement: Replacement::Text(text.into()),
        }
    }

    /// Replace `range` with protected text.
    #[must_use]
    pub fn protected(range: Range<usize>, text: impl Into<String>, kind: SpanKind) -> Self {
        Self {
            range,
            replacement: Replacement::Protected {
                text: text.into(),
                kind,
            },
        }
    }
}

/// Why a batch of [`Edit`]s was refused. Batches are all-or-nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// A range was out of bounds or not on a character boundary.
    InvalidRange(Range<usize>),
    /// Two ranges overlap.
    Overlap(Range<usize>),
    /// A range covers a protected span's placeholder.
    TouchesProtected(Range<usize>),
    /// Replacement text contained a placeholder character (it would forge a
    /// protected span).
    PlaceholderInText,
    /// The document already holds [`MAX_PROTECTED_SPANS`].
    TooManySpans,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange(r) => write!(f, "edit range {r:?} is not a valid char range"),
            Self::Overlap(r) => write!(f, "edit range {r:?} overlaps another edit"),
            Self::TouchesProtected(r) => write!(f, "edit range {r:?} covers a protected span"),
            Self::PlaceholderInText => write!(f, "replacement text contains a placeholder"),
            Self::TooManySpans => write!(f, "document is at its protected-span limit"),
        }
    }
}

impl std::error::Error for EditError {}

/// How formatter output violated a protected span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    /// Fewer intact copies than the input had: dropped or altered.
    Missing,
    /// More copies than the input had.
    Duplicated,
    /// All present, but in a different order.
    Reordered,
}

/// A protected span the formatter output did not preserve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanViolation {
    /// What went wrong.
    pub problem: ViolationKind,
    /// The span's kind.
    pub kind: SpanKind,
    /// The span's exact text.
    pub span: String,
}

impl fmt::Display for SpanViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.problem {
            ViolationKind::Missing => "is missing or altered in",
            ViolationKind::Duplicated => "is duplicated in",
            ViolationKind::Reordered => "is out of order in",
        };
        write!(
            f,
            "protected {} `{}` {what} the formatter output",
            self.kind.as_str(),
            self.span
        )
    }
}

impl std::error::Error for SpanViolation {}

/// Working text plus the spans no stage may alter.
#[derive(Debug, Clone, Default)]
pub struct TextDoc {
    text: String,
    spans: Vec<ProtectedSpan>,
    /// Reused output buffer, so a stage's rewrite does not allocate once the
    /// document has warmed up.
    scratch: String,
    /// Protection did not fit; see the module docs.
    raw: bool,
}

impl TextDoc {
    /// A document with no protected spans (beyond any private-use characters
    /// already in `input`, which are escaped as [`SpanKind::Literal`]).
    #[must_use]
    pub fn new(input: &str) -> Self {
        let mut doc = Self {
            text: String::with_capacity(input.len() + 8),
            spans: Vec::new(),
            scratch: String::new(),
            raw: false,
        };
        let literals = input.chars().filter(|c| is_placeholder(*c)).count();
        if literals == 0 {
            doc.text.push_str(input);
        } else if literals > MAX_PROTECTED_SPANS {
            // Escaping them all would need more placeholders than exist, and
            // leaving one unescaped would alias a placeholder. Keep the input
            // byte-for-byte instead.
            doc.text.push_str(input);
            doc.raw = true;
        } else {
            for c in input.chars() {
                if is_placeholder(c) {
                    doc.text.push(placeholder_for(doc.spans.len()));
                    doc.spans.push(ProtectedSpan {
                        kind: SpanKind::Literal,
                        text: c.to_string(),
                    });
                } else {
                    doc.text.push(c);
                }
            }
        }
        doc
    }

    /// A document with every built-in detector's spans protected and no other
    /// change — the guard the LLM pass needs even when the rules are off.
    #[must_use]
    pub fn protected(input: &str) -> Self {
        let mut doc = Self::new(input);
        let found = super::protect::detect_spans_in(&doc);
        doc.protect_ranges(&found);
        doc
    }

    /// Whether protection did not fit and the document is frozen as it was
    /// (see the module docs). Every stage leaves a raw document unchanged.
    #[must_use]
    pub fn is_raw(&self) -> bool {
        self.raw
    }

    /// Whether `c` is the placeholder of an escaped private-use character.
    /// Detection reads through these: a literal inside a URL is part of the
    /// URL, not a hole in its protection.
    pub(crate) fn is_literal_placeholder(&self, c: char) -> bool {
        self.span_for(c)
            .is_some_and(|s| s.kind == SpanKind::Literal)
    }

    /// The text stages edit, with each protected span as one placeholder.
    #[must_use]
    pub fn working_text(&self) -> &str {
        &self.text
    }

    /// Every span this document protects, indexed by placeholder. A span whose
    /// placeholder a stage removed wholesale would still be listed; the
    /// built-in rules never do that (see the module docs).
    #[must_use]
    pub fn spans(&self) -> &[ProtectedSpan] {
        &self.spans
    }

    /// The span behind a placeholder character, if `c` is one.
    #[must_use]
    pub fn span_for(&self, c: char) -> Option<&ProtectedSpan> {
        placeholder_index(c).and_then(|i| self.spans.get(i))
    }

    /// The spans in the order their placeholders appear in the working text.
    pub fn spans_in_text_order(&self) -> impl Iterator<Item = &ProtectedSpan> + '_ {
        self.text.chars().filter_map(|c| self.span_for(c))
    }

    /// Byte ranges of every protected span in [`restore`](Self::restore)'s
    /// output, in text order. The LLM pass masks exactly these.
    #[must_use]
    pub fn protected_byte_ranges(&self) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut at = 0;
        for c in self.text.chars() {
            match self.span_for(c) {
                Some(span) => {
                    ranges.push(at..at + span.text.len());
                    at += span.text.len();
                }
                None => at += c.len_utf8(),
            }
        }
        ranges
    }

    /// The finished text: every placeholder replaced by its original bytes.
    #[must_use]
    pub fn restore(&self) -> String {
        let mut out = String::with_capacity(self.text.len() + 64);
        self.restore_into(&mut out);
        out
    }

    /// [`restore`](Self::restore) into an existing buffer (cleared first).
    pub fn restore_into(&self, out: &mut String) {
        out.clear();
        if self.spans.is_empty() {
            out.push_str(&self.text);
            return;
        }
        let mut last = 0;
        for (i, c) in self.text.char_indices() {
            if let Some(span) = self.span_for(c) {
                out.push_str(&self.text[last..i]);
                out.push_str(&span.text);
                last = i + c.len_utf8();
            }
        }
        out.push_str(&self.text[last..]);
    }

    /// Protect byte ranges of the working text in place.
    ///
    /// Ranges must be sorted, non-overlapping, non-empty and on char
    /// boundaries, and may contain no placeholder except escaped literals
    /// (which become part of the new span); any that are not are skipped
    /// (returned count excludes them). Detectors produce ranges that satisfy
    /// all of this.
    ///
    /// If the valid ranges do not all fit in the placeholder range, nothing
    /// is protected and the document becomes raw ([`is_raw`](Self::is_raw)).
    pub fn protect_ranges(&mut self, ranges: &[(Range<usize>, SpanKind)]) -> usize {
        if ranges.is_empty() || self.raw {
            return 0;
        }
        let mut valid = Vec::with_capacity(ranges.len());
        let mut last = 0;
        for (range, kind) in ranges {
            let ok = range.start >= last
                && range.start < range.end
                && range.end <= self.text.len()
                && self.text.is_char_boundary(range.start)
                && self.text.is_char_boundary(range.end)
                && self.text[range.clone()]
                    .chars()
                    .all(|c| !is_placeholder(c) || self.is_literal_placeholder(c));
            if ok {
                valid.push((range.clone(), *kind));
                last = range.end;
            }
        }
        if valid.is_empty() {
            return 0;
        }
        if self.spans.len() + valid.len() > MAX_PROTECTED_SPANS {
            self.raw = true;
            return 0;
        }
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        let mut last = 0;
        for (range, kind) in &valid {
            out.push_str(&self.text[last..range.start]);
            let mut text = String::with_capacity(range.len());
            for c in self.text[range.clone()].chars() {
                match self.span_for(c) {
                    // An escaped literal: its original character joins the span.
                    Some(literal) => text.push_str(&literal.text),
                    None => text.push(c),
                }
            }
            out.push(placeholder_for(self.spans.len()));
            self.spans.push(ProtectedSpan { kind: *kind, text });
            last = range.end;
        }
        out.push_str(&self.text[last..]);
        self.scratch = std::mem::replace(&mut self.text, out);
        valid.len()
    }

    /// Apply a batch of range edits to the working text, all or nothing.
    ///
    /// This is the integration point for stages with their own matchers: S22
    /// replaces a mis-heard phrase with a protected canonical term, S24 a
    /// trigger phrase with a protected snippet placeholder. An edit may never
    /// cover an existing placeholder — protected spans stay opaque to every
    /// stage after the one that protected them.
    ///
    /// # Errors
    ///
    /// Any invalid edit rejects the whole batch; the document is unchanged.
    pub fn apply_edits(&mut self, mut edits: Vec<Edit>) -> Result<usize, EditError> {
        if self.raw {
            return Err(EditError::TooManySpans);
        }
        edits.sort_by_key(|e| e.range.start);
        let mut last = 0;
        let mut new_spans = 0;
        for e in &edits {
            let r = &e.range;
            if r.start > r.end
                || r.end > self.text.len()
                || !self.text.is_char_boundary(r.start)
                || !self.text.is_char_boundary(r.end)
            {
                return Err(EditError::InvalidRange(r.clone()));
            }
            if r.start < last {
                return Err(EditError::Overlap(r.clone()));
            }
            if self.text[r.clone()].chars().any(is_placeholder) {
                return Err(EditError::TouchesProtected(r.clone()));
            }
            let text = match &e.replacement {
                Replacement::Text(t) => t,
                Replacement::Protected { text, .. } => {
                    new_spans += 1;
                    text
                }
            };
            if text.chars().any(is_placeholder) {
                return Err(EditError::PlaceholderInText);
            }
            last = r.end;
        }
        if self.spans.len() + new_spans > MAX_PROTECTED_SPANS {
            return Err(EditError::TooManySpans);
        }
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        let mut last = 0;
        for e in edits.iter() {
            out.push_str(&self.text[last..e.range.start]);
            match &e.replacement {
                Replacement::Text(t) => out.push_str(t),
                Replacement::Protected { text, kind } => {
                    out.push(placeholder_for(self.spans.len()));
                    self.spans.push(ProtectedSpan {
                        kind: *kind,
                        text: text.clone(),
                    });
                }
            }
            last = e.range.end;
        }
        out.push_str(&self.text[last..]);
        self.scratch = std::mem::replace(&mut self.text, out);
        Ok(edits.len())
    }

    /// Check formatter (LLM) output against this document's protected spans.
    ///
    /// `output` is plain text, the way the LLM returns it — the LLM is given
    /// [`restore`](Self::restore)d text, never placeholders. Both texts are
    /// read as a stream of protected tokens: each occurrence of a protected
    /// span's exact bytes that stands as a whole token, as detection defines
    /// one (only opening brackets or quotes before it, only closing
    /// punctuation or a possessive `'s` after it, up to whitespace), matched
    /// leftmost-longest without overlap — so a path inside a protected code
    /// span is part of that span, not a second copy. The output passes only
    /// if its stream is exactly the input's: nothing dropped or altered
    /// (`/research_codebase` → `research_codebase` or `/research_codebases`),
    /// nothing extended into a different token (`…/a` → `…/a?q=x`), nothing
    /// added, nothing reordered.
    ///
    /// Linear in the two texts: one automaton over the distinct spans, one
    /// pass over each text.
    ///
    /// # Errors
    ///
    /// The first span that is missing/altered, duplicated, or reordered.
    pub fn verify_output(&self, output: &str) -> Result<(), SpanViolation> {
        let restored = self.restore();
        if self.raw {
            return identical(&restored, output, SpanKind::Literal, "the document");
        }
        let occurrences = self.protected_occurrences();
        let Some(index) = SpanIndex::new(&occurrences) else {
            return Ok(());
        };
        let expected = index.scan(&restored);
        // Every protected occurrence must itself read as a whole token, or
        // the stream could not hold it. A span a stage glued to its
        // neighbours can only be checked by demanding identical output.
        let mut cursor = 0;
        for (range, span) in &occurrences {
            while expected
                .get(cursor)
                .is_some_and(|(_, r)| r.start < range.start)
            {
                cursor += 1;
            }
            if expected.get(cursor).map(|(_, r)| r) != Some(range) {
                return identical(&restored, output, span.kind, &span.text);
            }
        }
        let actual = index.scan(output);
        if expected
            .iter()
            .map(|(id, _)| id)
            .eq(actual.iter().map(|(id, _)| id))
        {
            return Ok(());
        }
        let mut balance = vec![0isize; index.texts.len()];
        for (id, _) in &expected {
            balance[*id] += 1;
        }
        for (id, _) in &actual {
            balance[*id] -= 1;
        }
        let violation = |problem, id: usize| SpanViolation {
            problem,
            kind: index.kinds[id],
            span: index.texts[id].to_string(),
        };
        // A lost span is the more useful report: an unwrapped code span reads
        // as one copy missing, not as its contents duplicated.
        if let Some((id, _)) = expected.iter().find(|(id, _)| balance[*id] > 0) {
            return Err(violation(ViolationKind::Missing, *id));
        }
        if let Some((id, _)) = expected.iter().find(|(id, _)| balance[*id] < 0) {
            return Err(violation(ViolationKind::Duplicated, *id));
        }
        let first_difference = expected
            .iter()
            .zip(&actual)
            .find(|((a, _), (b, _))| a != b)
            .map_or(expected[0].0, |((a, _), _)| *a);
        Err(violation(ViolationKind::Reordered, first_difference))
    }

    /// Each protected span with its byte range in [`restore`](Self::restore)'s
    /// output, in text order.
    fn protected_occurrences(&self) -> Vec<(Range<usize>, &ProtectedSpan)> {
        let mut out = Vec::new();
        let mut at = 0;
        for c in self.text.chars() {
            match self.span_for(c) {
                Some(span) => {
                    out.push((at..at + span.text.len(), span));
                    at += span.text.len();
                }
                None => at += c.len_utf8(),
            }
        }
        out
    }

    /// Replace the whole working text, placeholders and all. Only for the
    /// hallucination scrub, which edits whitespace and known artifacts around
    /// the placeholders and drops only the placeholder of a protected
    /// artifact (a trailing `/no_think`); it never adds or reorders one.
    pub(crate) fn rewrite_unprotected(&mut self, f: impl FnOnce(&str, &mut String)) {
        if self.raw {
            return;
        }
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        f(&self.text, &mut out);
        debug_assert!(
            out.chars().filter(|c| is_placeholder(*c)).count()
                <= self.text.chars().filter(|c| is_placeholder(*c)).count(),
            "a plain rewrite must never forge a placeholder"
        );
        if out != self.text {
            self.scratch = std::mem::replace(&mut self.text, out);
        } else {
            self.scratch = out;
        }
    }

    /// Run a token-level edit (the built-in rules' editing surface).
    pub(crate) fn edit(&mut self, f: impl FnOnce(&mut Editor<'_>)) {
        if self.raw {
            return;
        }
        let text = std::mem::take(&mut self.text);
        let mut out = std::mem::take(&mut self.scratch);
        let changed = {
            let mut ed = Editor::new(&text, &mut self.spans);
            f(&mut ed);
            ed.finish(&mut out)
        };
        if changed {
            self.text = out;
            self.scratch = text;
        } else {
            self.text = text;
            self.scratch = out;
        }
    }

    /// Rewrite the text of an existing span in place.
    ///
    /// Crate-private on purpose: only the built-in acoustic corrections may do
    /// this, to fix Whisper's `.cloud/` → `.claude/` inside a detected path.
    /// No external stage can amend a span another stage protected.
    pub(crate) fn amend_span(&mut self, index: usize, text: String) {
        if self.raw {
            return;
        }
        if let Some(span) = self.spans.get_mut(index) {
            span.text = text;
        }
    }
}

/// Output that must equal the restored text exactly, because the document
/// cannot be verified span by span.
fn identical(
    restored: &str,
    output: &str,
    kind: SpanKind,
    span: &str,
) -> Result<(), SpanViolation> {
    if output == restored {
        Ok(())
    } else {
        Err(SpanViolation {
            problem: ViolationKind::Missing,
            kind,
            span: span.to_string(),
        })
    }
}

/// The distinct protected texts of a document, searchable in one pass.
struct SpanIndex<'a> {
    texts: Vec<&'a str>,
    kinds: Vec<SpanKind>,
    automaton: AhoCorasick,
}

impl<'a> SpanIndex<'a> {
    fn new(occurrences: &[(Range<usize>, &'a ProtectedSpan)]) -> Option<Self> {
        let mut ids: HashMap<&str, usize> = HashMap::new();
        let mut texts = Vec::new();
        let mut kinds = Vec::new();
        for (_, span) in occurrences {
            if span.text.is_empty() {
                continue;
            }
            ids.entry(span.text.as_str()).or_insert_with(|| {
                texts.push(span.text.as_str());
                kinds.push(span.kind);
                texts.len() - 1
            });
        }
        if texts.is_empty() {
            return None;
        }
        let automaton = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&texts)
            .expect("an automaton over a document's spans fits its default limits");
        Some(Self {
            texts,
            kinds,
            automaton,
        })
    }

    /// The protected tokens of `hay`, leftmost-longest, non-overlapping.
    fn scan(&self, hay: &str) -> Vec<(usize, Range<usize>)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < hay.len() {
            let Some(m) = self.automaton.find(Input::new(hay).span(at..hay.len())) else {
                break;
            };
            let id = m.pattern().as_usize();
            if whole_token(hay, m.start(), m.end(), self.kinds[id]) {
                out.push((id, m.range()));
                at = m.end();
            } else {
                at = m.start() + hay[m.start()..].chars().next().map_or(1, char::len_utf8);
            }
        }
        out
    }
}

/// Whether `hay[start..end]` is a token of its own, by the rule detection
/// uses to cut one out of a whitespace-separated chunk: only openers between
/// it and the whitespace before, only closing punctuation (after an optional
/// possessive `'s`) between it and the whitespace after. A backticked code
/// span delimits itself; a literal is a single character.
fn whole_token(hay: &str, start: usize, end: usize, kind: SpanKind) -> bool {
    use super::protect::{is_closer, OPENERS};
    let span = &hay[start..end];
    if kind == SpanKind::Literal
        || (span.len() >= 2 && span.starts_with('`') && span.ends_with('`'))
    {
        return true;
    }
    let before_ok = hay[..start]
        .chars()
        .rev()
        .take_while(|c| !c.is_whitespace())
        .all(|c| OPENERS.contains(&c));
    if !before_ok {
        return false;
    }
    let mut rest = &hay[end..];
    for possessive in ["'s", "\u{2019}s"] {
        if let Some(r) = rest.strip_prefix(possessive) {
            if r.chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || is_closer(c))
            {
                rest = r;
                break;
            }
        }
    }
    rest.chars()
        .take_while(|c| !c.is_whitespace())
        .all(is_closer)
}

#[cfg(test)]
mod tests {
    #[test]
    fn protected_byte_ranges_index_the_restored_text() {
        // Multi-byte text before, between and inside spans: offsets must be
        // byte offsets of the restored string, not of the working text.
        let doc = TextDoc::protected("café: run /create_plan then open ~/x/é.rs — ok");
        let out = doc.restore();
        let spans: Vec<&str> = doc
            .protected_byte_ranges()
            .into_iter()
            .map(|r| &out[r])
            .collect();
        assert_eq!(spans, vec!["/create_plan", "~/x/é.rs"]);
        assert!(TextDoc::protected("no spans here")
            .protected_byte_ranges()
            .is_empty());
    }

    use super::*;

    fn doc_with(text: &str, spans: &[(&str, SpanKind)]) -> TextDoc {
        let mut ranges = Vec::new();
        for (s, kind) in spans {
            let start = text.find(s).expect("span in text");
            ranges.push((start..start + s.len(), *kind));
        }
        ranges.sort_by_key(|(r, _)| r.start);
        let mut doc = TextDoc::new(text);
        assert_eq!(doc.protect_ranges(&ranges), spans.len());
        doc
    }

    #[test]
    fn protecting_replaces_each_span_with_one_opaque_placeholder() {
        let doc = doc_with(
            "run /research_codebase on ~/.claude/x now",
            &[
                ("/research_codebase", SpanKind::SlashCommand),
                ("~/.claude/x", SpanKind::Path),
            ],
        );
        let working = doc.working_text();
        assert!(
            !working.contains('/'),
            "spans are fully cut out: {working:?}"
        );
        assert_eq!(working.chars().filter(|c| is_placeholder(*c)).count(), 2);
        assert!(working.starts_with("run ") && working.ends_with(" now"));
        assert_eq!(doc.restore(), "run /research_codebase on ~/.claude/x now");
    }

    #[test]
    fn preexisting_private_use_characters_round_trip_as_literals() {
        let input = "a \u{F0000} b \u{F0003}";
        let doc = TextDoc::new(input);
        assert_eq!(doc.spans().len(), 2);
        assert!(doc.spans().iter().all(|s| s.kind == SpanKind::Literal));
        assert_eq!(doc.restore(), input);
        assert_eq!(doc.verify_output(input), Ok(()));
    }

    #[test]
    fn invalid_protect_ranges_are_skipped_not_applied() {
        let mut doc = TextDoc::new("héllo world");
        // 2 is inside the two-byte 'é'.
        assert_eq!(doc.protect_ranges(&[(0..2, SpanKind::Code)]), 0);
        assert_eq!(doc.restore(), "héllo world");
    }

    #[test]
    fn edits_are_all_or_nothing_and_never_touch_placeholders() {
        let mut doc = doc_with(
            "say /create_plan please",
            &[("/create_plan", SpanKind::SlashCommand)],
        );
        let p = doc.working_text().find(is_placeholder).unwrap();
        let bad = vec![
            Edit::text(0..3, "tell"),
            Edit::text(p..p + 4, "x"), // covers the placeholder
        ];
        assert_eq!(
            doc.apply_edits(bad),
            Err(EditError::TouchesProtected(p..p + 4))
        );
        assert_eq!(doc.restore(), "say /create_plan please", "nothing applied");

        let forged = vec![Edit::text(0..3, "\u{F0000}")];
        assert_eq!(doc.apply_edits(forged), Err(EditError::PlaceholderInText));

        let good = vec![
            Edit::protected(0..3, "Claude", SpanKind::Term),
            Edit::text(
                doc.working_text().len() - 6..doc.working_text().len(),
                "now",
            ),
        ];
        assert_eq!(doc.apply_edits(good), Ok(2));
        assert_eq!(doc.restore(), "Claude /create_plan now");
        assert_eq!(doc.spans().len(), 2);
    }

    #[test]
    fn verify_accepts_output_that_keeps_every_span_intact() {
        let doc = doc_with(
            "run /research_codebase then /create_plan",
            &[
                ("/research_codebase", SpanKind::SlashCommand),
                ("/create_plan", SpanKind::SlashCommand),
            ],
        );
        assert_eq!(
            doc.verify_output("Run /research_codebase, then /create_plan."),
            Ok(())
        );
    }

    #[test]
    fn verify_rejects_the_production_slash_strip() {
        let doc = doc_with(
            "run /research_codebase on the auth module",
            &[("/research_codebase", SpanKind::SlashCommand)],
        );
        let err = doc
            .verify_output("Run research_codebase on the auth module.")
            .unwrap_err();
        assert_eq!(err.problem, ViolationKind::Missing);
        assert_eq!(err.span, "/research_codebase");
        assert!(err.to_string().contains("`/research_codebase`"));
    }

    #[test]
    fn verify_rejects_a_span_extended_into_a_different_token() {
        let doc = doc_with("open ~/.claude now", &[("~/.claude", SpanKind::Path)]);
        for bad in [
            "Open ~/.claude/settings now.",
            "Open ~/.claudex now.",
            "Open x~/.claude now.",
            "Open ~/.claude.bak now.",
        ] {
            assert!(doc.verify_output(bad).is_err(), "{bad}");
        }
        assert_eq!(doc.verify_output("Open ~/.claude now."), Ok(()));
        assert_eq!(doc.verify_output("Open (~/.claude) now"), Ok(()));
    }

    #[test]
    fn verify_rejects_duplicates_and_reordering() {
        let doc = doc_with(
            "run /a_one then /b_two",
            &[
                ("/a_one", SpanKind::SlashCommand),
                ("/b_two", SpanKind::SlashCommand),
            ],
        );
        assert_eq!(
            doc.verify_output("run /a_one then /b_two and /b_two")
                .unwrap_err()
                .problem,
            ViolationKind::Duplicated
        );
        assert_eq!(
            doc.verify_output("run /b_two then /a_one")
                .unwrap_err()
                .problem,
            ViolationKind::Reordered
        );
    }

    #[test]
    fn verify_counts_repeated_spans() {
        let doc = doc_with("x_y and x_y", &[("x_y", SpanKind::Code)]);
        // Only the first occurrence was protected by doc_with; the second is a
        // plain-text copy and still counts toward the expected total.
        assert_eq!(doc.verify_output("x_y and x_y"), Ok(()));
        assert_eq!(
            doc.verify_output("x_y and").unwrap_err().problem,
            ViolationKind::Missing
        );
    }

    /// `VERIFIER_REJECTS_IDENTITY_OUTPUT`: occurrences inside another
    /// protected span are not separate copies; a byte-identical answer always
    /// verifies.
    #[test]
    fn identity_output_always_verifies() {
        for input in [
            "open /tmp/x then `/tmp/x`",
            "see `user_id` and user_id and `user_id`",
            "run /a_b then ``/a_b `x` /a_b`` and /a_b",
            "open ~/.claude/x and `cat ~/.claude/x` twice: ~/.claude/x",
        ] {
            let doc = TextDoc::protected(input);
            assert!(!doc.spans().is_empty(), "{input:?}");
            assert_eq!(doc.verify_output(&doc.restore()), Ok(()), "{input:?}");
        }
        // Repeated dictionary terms, protected and plain, nested in code.
        let mut doc = TextDoc::protected("ask cloud and cloud about `Claude` and Claude");
        let text = doc.working_text().to_string();
        let edits = text
            .match_indices("cloud")
            .map(|(i, m)| Edit::protected(i..i + m.len(), "Claude", SpanKind::Term))
            .collect();
        doc.apply_edits(edits).unwrap();
        let out = doc.restore();
        assert_eq!(out, "ask Claude and Claude about `Claude` and Claude");
        assert_eq!(doc.verify_output(&out), Ok(()));
        assert_eq!(
            doc.verify_output("ask Claude and Claude about Claude and Claude")
                .unwrap_err()
                .kind,
            SpanKind::Code,
            "unwrapping the code span is caught"
        );
        assert!(doc
            .verify_output("ask Claude about `Claude` and Claude")
            .is_err());
    }

    /// `VERIFIER_ACCEPTS_CHANGED_URL`: a span followed by more token
    /// characters is a different token; only openers before and sentence
    /// punctuation after are allowed, as in detection.
    #[test]
    fn a_span_continued_into_a_different_token_is_rejected() {
        let doc = TextDoc::protected("see https://example.com/a and user_id now");
        for bad in [
            "See https://example.com/a?q=changed and user_id now.",
            "See https://example.com/a#changed and user_id now.",
            "See https://example.com/a&x=1 and user_id now.",
            "See https://example.com/a%20 and user_id now.",
            "See https://example.com/a+b and user_id now.",
            "See https://example.com/a and user_id#changed now.",
            "See https://example.com/a and user_id=1 now.",
            "See https://example.com/a and user_id* now.",
            "See *https://example.com/a and user_id now.",
        ] {
            assert!(doc.verify_output(bad).is_err(), "{bad}");
        }
        for good in [
            "See https://example.com/a and user_id now.",
            "See (https://example.com/a), and \"user_id\".",
            "See https://example.com/a. And user_id's value now!",
            "See https://example.com/a; user_id: now?",
        ] {
            assert_eq!(doc.verify_output(good), Ok(()), "{good}");
        }
    }

    /// `LITERAL_PUA_BYPASSES_PROTECTION`: a private-use character inside a
    /// URL, path or code span is part of that span, not a hole in it.
    #[test]
    fn a_literal_private_use_character_does_not_unprotect_its_token() {
        for (input, span) in [
            (
                "https://example.com/\u{F0000}",
                "https://example.com/\u{F0000}",
            ),
            ("open ~/x/\u{F0001}/y.rs now", "~/x/\u{F0001}/y.rs"),
            ("run `a \u{F0002} b` now", "`a \u{F0002} b`"),
            (
                "see \u{F0003}https://example.com/x now",
                "\u{F0003}https://example.com/x",
            ),
        ] {
            let doc = TextDoc::protected(input);
            assert_eq!(doc.restore(), input);
            assert!(
                doc.spans_in_text_order().any(|s| s.text == span),
                "{input:?}: {:?}",
                doc.spans()
            );
            assert_eq!(doc.verify_output(input), Ok(()), "{input:?}");
        }
        let doc = TextDoc::protected("https://example.com/\u{F0000}");
        assert!(doc.verify_output("https://evil.example/\u{F0000}").is_err());
        let doc = TextDoc::protected("open ~/x/\u{F0001}/y.rs now");
        assert!(doc.verify_output("open ~/z/\u{F0001}/y.rs now").is_err());
    }

    /// `SPAN_CAPACITY_CORRUPTS_TEXT`: one literal more than the placeholder
    /// range holds must still round-trip, and must not alias a placeholder.
    #[test]
    fn literals_past_the_span_limit_round_trip() {
        let mut input = "\u{F0000}".repeat(MAX_PROTECTED_SPANS);
        input.push('\u{F0001}');
        let doc = TextDoc::new(&input);
        assert_eq!(doc.restore(), input);
        assert_eq!(doc.verify_output(&input), Ok(()));
        assert!(doc.verify_output("changed").is_err());
        // At exactly the limit everything is still a literal span.
        let at_cap = "\u{F0001}".repeat(MAX_PROTECTED_SPANS);
        assert_eq!(TextDoc::new(&at_cap).restore(), at_cap);
    }

    /// `SPAN_CAPACITY_CORRUPTS_TEXT`: protection that does not fit leaves the
    /// document unchanged by every later edit instead of half-protected.
    #[test]
    fn protection_past_the_limit_freezes_the_document() {
        let mut input = "`x` ".repeat(MAX_PROTECTED_SPANS);
        input.push_str("`um` and uh");
        let doc = TextDoc::protected(&input);
        assert_eq!(doc.restore(), input);
        assert!(doc.is_raw());
        assert_eq!(doc.verify_output(&input), Ok(()));
        assert!(doc.verify_output(&input.replace("`um`", "``")).is_err());
        let mut doc = doc;
        assert_eq!(
            doc.apply_edits(vec![Edit::text(0..0, "x")]),
            Err(EditError::TooManySpans)
        );
        assert_eq!(doc.restore(), input);
    }
}
