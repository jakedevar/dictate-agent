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

use std::fmt;
use std::ops::Range;

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
fn placeholder_for(index: usize) -> char {
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
        };
        if input.chars().any(is_placeholder) {
            for c in input.chars() {
                if is_placeholder(c) && doc.spans.len() < MAX_PROTECTED_SPANS {
                    let p = placeholder_for(doc.spans.len());
                    doc.spans.push(ProtectedSpan {
                        kind: SpanKind::Literal,
                        text: c.to_string(),
                    });
                    doc.text.push(p);
                } else {
                    doc.text.push(c);
                }
            }
        } else {
            doc.text.push_str(input);
        }
        doc
    }

    /// A document with every built-in detector's spans protected and no other
    /// change — the guard the LLM pass needs even when the rules are off.
    #[must_use]
    pub fn protected(input: &str) -> Self {
        let mut doc = Self::new(input);
        let found = super::protect::detect_spans(doc.working_text());
        doc.protect_ranges(&found);
        doc
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
    /// Ranges must be sorted, non-overlapping, on char boundaries, and free of
    /// placeholders; any that are not are skipped (returned count excludes
    /// them). Detectors produce ranges that satisfy all four.
    pub fn protect_ranges(&mut self, ranges: &[(Range<usize>, SpanKind)]) -> usize {
        if ranges.is_empty() {
            return 0;
        }
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        let mut last = 0;
        let mut protected = 0;
        for (range, kind) in ranges {
            let valid = range.start >= last
                && range.start < range.end
                && range.end <= self.text.len()
                && self.text.is_char_boundary(range.start)
                && self.text.is_char_boundary(range.end)
                && !self.text[range.clone()].chars().any(is_placeholder)
                && self.spans.len() < MAX_PROTECTED_SPANS;
            if !valid {
                continue;
            }
            out.push_str(&self.text[last..range.start]);
            out.push(placeholder_for(self.spans.len()));
            self.spans.push(ProtectedSpan {
                kind: *kind,
                text: self.text[range.clone()].to_string(),
            });
            last = range.end;
            protected += 1;
        }
        out.push_str(&self.text[last..]);
        self.scratch = std::mem::replace(&mut self.text, out);
        protected
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
    /// [`restore`](Self::restore)d text, never placeholders. The output passes
    /// only if every protected span appears in it byte-for-byte, as a whole
    /// token (so `/research_codebase` → `research_codebase` or
    /// `/research_codebases` both fail), exactly as many times as the input
    /// carried it, and in the same relative order.
    ///
    /// # Errors
    ///
    /// The first span that is missing/altered, duplicated, or reordered.
    pub fn verify_output(&self, output: &str) -> Result<(), SpanViolation> {
        let ordered: Vec<&ProtectedSpan> = self.spans_in_text_order().collect();
        if ordered.is_empty() {
            return Ok(());
        }
        // Counts: expected copies = protected occurrences + any plain-text
        // occurrences outside the placeholders (normally zero).
        let mut seen: Vec<&str> = Vec::new();
        for span in &ordered {
            if seen.contains(&span.text.as_str()) {
                continue;
            }
            seen.push(&span.text);
            let protected = ordered.iter().filter(|s| s.text == span.text).count();
            let plain = count_whole(&self.text, &span.text, span.kind, true);
            let expected = protected + plain;
            let actual = count_whole(output, &span.text, span.kind, false);
            let problem = match actual.cmp(&expected) {
                std::cmp::Ordering::Less => Some(ViolationKind::Missing),
                std::cmp::Ordering::Greater => Some(ViolationKind::Duplicated),
                std::cmp::Ordering::Equal => None,
            };
            if let Some(problem) = problem {
                return Err(SpanViolation {
                    problem,
                    kind: span.kind,
                    span: span.text.clone(),
                });
            }
        }
        // Order: each span must be found after the previous one.
        let mut pos = 0;
        for span in &ordered {
            match find_whole(output, &span.text, span.kind, pos, false) {
                Some(end) => pos = end,
                None => {
                    return Err(SpanViolation {
                        problem: ViolationKind::Reordered,
                        kind: span.kind,
                        span: span.text.clone(),
                    })
                }
            }
        }
        Ok(())
    }

    /// Replace the whole working text. Only for stages that run before
    /// anything is protected (the hallucination scrub); the new text must keep
    /// every existing placeholder exactly once.
    pub(crate) fn rewrite_unprotected(&mut self, f: impl FnOnce(&str, &mut String)) {
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        f(&self.text, &mut out);
        debug_assert_eq!(
            out.chars().filter(|c| is_placeholder(*c)).count(),
            self.text.chars().filter(|c| is_placeholder(*c)).count(),
            "a plain rewrite must keep every placeholder"
        );
        if out != self.text {
            self.scratch = std::mem::replace(&mut self.text, out);
        } else {
            self.scratch = out;
        }
    }

    /// Run a token-level edit (the built-in rules' editing surface).
    pub(crate) fn edit(&mut self, f: impl FnOnce(&mut Editor<'_>)) {
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
        if let Some(span) = self.spans.get_mut(index) {
            span.text = text;
        }
    }
}

/// Characters that, directly before a span, would make it part of a longer
/// token (`x/research_codebase`, `~/usr/bin`).
fn glues_before(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '/' | '\\' | '.' | '~' | '-' | '@' | '#' | '$')
}

/// Characters that, directly after a span, would extend it
/// (`/research_codebases`, `~/.claude/x`). A `.` counts only when another
/// token character follows it — a sentence period after a span is fine.
fn glues_after(c: char, next: Option<char>) -> bool {
    c.is_alphanumeric()
        || matches!(c, '_' | '/' | '\\' | '-' | '@')
        || (matches!(c, '.' | ':') && next.is_some_and(|n| n.is_alphanumeric() || n == '_'))
}

fn whole_at(hay: &str, start: usize, end: usize, kind: SpanKind) -> bool {
    if kind == SpanKind::Literal {
        return true;
    }
    let before = hay[..start].chars().next_back();
    let mut after = hay[end..].chars();
    let a1 = after.next();
    let a2 = after.next();
    !before.is_some_and(glues_before) && !a1.is_some_and(|c| glues_after(c, a2))
}

/// End of the first whole-token occurrence of `needle` at or after `from`.
/// With `plain_only`, an occurrence that includes a placeholder is not one
/// (it is the protected copy itself, already counted).
fn find_whole(
    hay: &str,
    needle: &str,
    kind: SpanKind,
    from: usize,
    plain_only: bool,
) -> Option<usize> {
    let mut at = from;
    while let Some(off) = hay.get(at..).and_then(|h| h.find(needle)) {
        let start = at + off;
        let end = start + needle.len();
        let plain = !plain_only || !hay[start..end].chars().any(is_placeholder);
        if plain && whole_at(hay, start, end, kind) {
            return Some(end);
        }
        at = start + needle.chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn count_whole(hay: &str, needle: &str, kind: SpanKind, plain_only: bool) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut n = 0;
    let mut at = 0;
    while let Some(end) = find_whole(hay, needle, kind, at, plain_only) {
        n += 1;
        at = end;
    }
    n
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
}
