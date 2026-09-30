//! Protected spans: the model never sees them in a form it can rewrite.
//!
//! Three layers, cheapest first:
//!
//! 1. **Detach.** Protected spans at the very start of the text (a leading
//!    `/slash_command`, optionally followed by more spans such as a path) are
//!    cut off before the model is called and re-attached verbatim afterwards.
//!    The model never sees them at all — the observed production failure was
//!    exactly this position (`/research_codebase I would like…` →
//!    `Research codebase. I would like…`).
//! 2. **Mask.** Every other span is replaced by an opaque placeholder
//!    ([`MaskStyle`], chosen by evaluation) the prompt tells the model to copy.
//! 3. **Verify and restore.** Each placeholder must come back exactly once, in
//!    order, not glued to a new word character, with no placeholder debris
//!    left; otherwise the output is rejected and the pass fails open.
//!
//! The caller (S20's protect stage) supplies byte ranges. Because a missed
//! span is a P0 corruption and not a style nit, [`fallback_spans`] adds a
//! conservative detector for unmistakably technical tokens as defence in
//! depth; its ranges are unioned with the caller's.

use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;

/// Placeholder representation. See the research note for the evaluation
/// that picked the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskStyle {
    /// `⟦1⟧` — mathematical white square brackets. Rare in text, one or two
    /// tokens per bracket, and models treat it as an atom.
    Brackets,
    /// `<k1/>` — an XML-ish empty element.
    XmlTag,
    /// `ZQX1` — a nonsense capitalized word.
    Letters,
}

impl MaskStyle {
    /// Every style, for the evaluation sweep.
    pub const ALL: [MaskStyle; 3] = [MaskStyle::Brackets, MaskStyle::XmlTag, MaskStyle::Letters];

    /// Placeholder for span number `n` (1-based).
    #[must_use]
    pub fn token(self, n: usize) -> String {
        match self {
            Self::Brackets => format!("⟦{n}⟧"),
            Self::XmlTag => format!("<k{n}/>"),
            Self::Letters => format!("ZQX{n}"),
        }
    }

    /// Stable name (config, reports).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Brackets => "brackets",
            Self::XmlTag => "xml_tag",
            Self::Letters => "letters",
        }
    }

    /// Parse [`as_str`](Self::as_str).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    pub(crate) fn pattern(self) -> &'static Regex {
        static BRACKETS: OnceLock<Regex> = OnceLock::new();
        static XML: OnceLock<Regex> = OnceLock::new();
        static LETTERS: OnceLock<Regex> = OnceLock::new();
        match self {
            Self::Brackets => BRACKETS.get_or_init(|| Regex::new(r"⟦(\d+)⟧").unwrap()),
            Self::XmlTag => XML.get_or_init(|| Regex::new(r"<k(\d+)\s*/>").unwrap()),
            Self::Letters => LETTERS.get_or_init(|| Regex::new(r"ZQX(\d+)").unwrap()),
        }
    }

    /// Fragments that must not survive restoration: a mangled placeholder
    /// (`⟦1`, `<k1>`, `ZQX`) left in the text means the model damaged one.
    fn debris(self, text: &str) -> bool {
        match self {
            Self::Brackets => text.contains('⟦') || text.contains('⟧'),
            Self::XmlTag => {
                static DEBRIS: OnceLock<Regex> = OnceLock::new();
                DEBRIS
                    .get_or_init(|| Regex::new(r"</?k\d*\s*/?>").unwrap())
                    .is_match(text)
            }
            Self::Letters => text.contains("ZQX"),
        }
    }

    /// Whether `text` already contains something that would parse as a
    /// placeholder, which would make restoration ambiguous.
    #[must_use]
    pub fn collides_with(self, text: &str) -> bool {
        self.debris(text) || self.pattern().is_match(text)
    }
}

/// Why protected spans could not be prepared or restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanError {
    /// A caller range is out of bounds or not on a char boundary.
    InvalidRange(Range<usize>),
    /// The input already contains placeholder-like text.
    Collision,
    /// A placeholder is missing from the output.
    Dropped(usize),
    /// A placeholder appears more than once.
    Duplicated(usize),
    /// Placeholders came back in a different order.
    Reordered,
    /// A placeholder number the input never had.
    Unknown(usize),
    /// A mangled placeholder remains.
    Debris,
    /// A span was glued onto a word character it was not attached to.
    Glued(usize),
}

impl std::fmt::Display for SpanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRange(r) => write!(f, "invalid protected range {}..{}", r.start, r.end),
            Self::Collision => write!(f, "input already contains placeholder text"),
            Self::Dropped(n) => write!(f, "protected span {n} dropped"),
            Self::Duplicated(n) => write!(f, "protected span {n} duplicated"),
            Self::Reordered => write!(f, "protected spans reordered"),
            Self::Unknown(n) => write!(f, "unknown placeholder {n}"),
            Self::Debris => write!(f, "mangled placeholder in output"),
            Self::Glued(n) => write!(f, "protected span {n} glued to a word"),
        }
    }
}

/// Validate caller ranges, union the fallback detector's, sort and merge.
///
/// # Errors
///
/// [`SpanError::InvalidRange`] if a caller range is out of bounds, reversed,
/// or splits a UTF-8 character — a caller bug that must not be papered over.
pub fn normalize_spans(
    text: &str,
    caller: &[Range<usize>],
    fallback: bool,
) -> Result<Vec<Range<usize>>, SpanError> {
    let mut spans: Vec<Range<usize>> = Vec::with_capacity(caller.len());
    for r in caller {
        if r.start > r.end
            || r.end > text.len()
            || !text.is_char_boundary(r.start)
            || !text.is_char_boundary(r.end)
        {
            return Err(SpanError::InvalidRange(r.clone()));
        }
        if r.start < r.end {
            spans.push(r.clone());
        }
    }
    if fallback {
        spans.extend(fallback_spans(text));
    }
    spans.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for r in spans {
        match merged.last_mut() {
            // Overlapping (not merely adjacent) ranges merge; adjacent ones
            // stay separate so each keeps its own placeholder.
            Some(last) if r.start < last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    Ok(merged)
}

/// Words outside protected spans — what the model actually has to format.
#[must_use]
pub fn unprotected_words(text: &str, spans: &[Range<usize>]) -> usize {
    let mut count = 0;
    let mut pos = 0;
    for r in spans {
        count += text[pos..r.start].split_whitespace().count();
        pos = r.end;
    }
    count + text[pos..].split_whitespace().count()
}

/// Text prepared for the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Masked {
    /// Leading protected text (with its trailing whitespace) cut off before
    /// the model call and re-attached verbatim.
    pub prefix: String,
    /// What the model sees: the rest, spans replaced by placeholders.
    pub body: String,
    /// Original text of each masked span; placeholder `n` is `spans[n-1]`.
    pub spans: Vec<String>,
    /// Characters adjacent to each placeholder in `body` (before, after).
    boundaries: Vec<(Option<char>, Option<char>)>,
    pub style: MaskStyle,
}

/// Detach leading spans and mask the rest. `spans` must come from
/// [`normalize_spans`].
///
/// # Errors
///
/// [`SpanError::Collision`] if the text already contains placeholder text.
pub fn mask(text: &str, spans: &[Range<usize>], style: MaskStyle) -> Result<Masked, SpanError> {
    if style.collides_with(text) {
        return Err(SpanError::Collision);
    }

    // 1. Detach: consume spans that start the text and are followed by
    //    whitespace (or the end). "/commit, then…" is not detached — the
    //    comma belongs to the sentence — it is masked instead.
    let mut cut = text.len() - text.trim_start().len();
    let mut consumed = 0;
    for r in spans {
        if r.start != cut {
            break;
        }
        let rest = &text[r.end..];
        let ws = rest.len() - rest.trim_start().len();
        if ws == 0 && !rest.is_empty() {
            break;
        }
        // "/create_plan uh actually no /research_codebase …": the speaker
        // is correcting the span itself. Detached, the model could resolve
        // the correction by deleting the cue while the span stays — two
        // commands where one was meant. Masked instead, the span-correction
        // validator makes that case fail open.
        if follows_correction_cue(rest) {
            break;
        }
        cut = r.end + ws;
        consumed += 1;
    }
    let (prefix, body_start) = if consumed > 0 {
        (text[..cut].to_string(), cut)
    } else {
        (String::new(), 0)
    };

    // 2. Mask the remaining spans.
    let mut body = String::with_capacity(text.len() - body_start);
    let mut originals = Vec::new();
    let mut positions = Vec::new();
    let mut pos = body_start;
    for r in &spans[consumed..] {
        body.push_str(&text[pos..r.start]);
        originals.push(text[r.clone()].to_string());
        let start = body.len();
        body.push_str(&style.token(originals.len()));
        positions.push(start..body.len());
        pos = r.end;
    }
    body.push_str(&text[pos..]);

    let boundaries = positions
        .iter()
        .map(|p| (body[..p.start].chars().next_back(), body[p.end..].chars().next()))
        .collect();

    Ok(Masked {
        prefix,
        body,
        spans: originals,
        boundaries,
        style,
    })
}

impl Masked {
    /// After a detached command prefix the body is the command's argument
    /// ("/describe_pr and mention…"), not a new sentence: keep the first
    /// word's case as dictated. The pronoun "I" is the exception.
    fn keep_continuation_case(&self, output: &str) -> String {
        if self.prefix.is_empty() {
            return output.to_string();
        }
        let (Some(i), Some(o)) = (self.body.chars().next(), output.chars().next()) else {
            return output.to_string();
        };
        let first_word = self
            .body
            .split(|c: char| c.is_whitespace() || c == ',')
            .next()
            .unwrap_or_default()
            .to_lowercase();
        let pronoun = matches!(first_word.as_str(), "i" | "i'm" | "i'll" | "i've" | "i'd");
        if i != o && i.to_lowercase().eq(o.to_lowercase()) && !pronoun {
            let mut s = String::with_capacity(output.len());
            s.push(i);
            s.push_str(&output[o.len_utf8()..]);
            s
        } else {
            output.to_string()
        }
    }

    /// Verify the model's output and put the spans back. Returns the full
    /// text: prefix + restored body.
    ///
    /// # Errors
    ///
    /// Any [`SpanError`] — the output must then be rejected.
    pub fn restore(&self, output: &str) -> Result<String, SpanError> {
        let found: Vec<(Range<usize>, usize)> = self
            .style
            .pattern()
            .captures_iter(output)
            .map(|c| {
                let m = c.get(0).expect("whole match");
                let n = c[1].parse::<usize>().unwrap_or(usize::MAX);
                (m.range(), n)
            })
            .collect();

        let mut seen = vec![false; self.spans.len()];
        let mut last = 0;
        for (_, n) in &found {
            if *n == 0 || *n > self.spans.len() {
                return Err(SpanError::Unknown(*n));
            }
            if seen[n - 1] {
                return Err(SpanError::Duplicated(*n));
            }
            seen[n - 1] = true;
            if *n < last {
                return Err(SpanError::Reordered);
            }
            last = *n;
        }
        if let Some(i) = seen.iter().position(|s| !s) {
            return Err(SpanError::Dropped(i + 1));
        }

        let mut restored = String::with_capacity(output.len() + self.prefix.len() + 64);
        restored.push_str(&self.prefix);
        let output = &self.keep_continuation_case(output);
        let mut pos = 0;
        for (range, n) in &found {
            let before = output[..range.start].chars().next_back();
            let after = output[range.end..].chars().next();
            let (in_before, in_after) = self.boundaries[n - 1];
            if glued(before, in_before, GLUE_OK_BEFORE) || glued(after, in_after, GLUE_OK_AFTER) {
                return Err(SpanError::Glued(*n));
            }
            restored.push_str(&output[pos..range.start]);
            restored.push_str(&self.spans[n - 1]);
            pos = range.end;
        }
        let tail = &output[pos..];
        restored.push_str(tail);

        // Debris check on the model's own text only: a span's original text
        // may legitimately contain anything.
        let mut model_text = String::new();
        let mut p = 0;
        for (range, _) in &found {
            model_text.push_str(&output[p..range.start]);
            model_text.push(' ');
            p = range.end;
        }
        model_text.push_str(&output[p..]);
        if self.style.debris(&model_text) {
            return Err(SpanError::Debris);
        }
        Ok(restored)
    }
}

const FILLERS: &[&str] = &["uh", "um", "er", "erm", "oh", "like", "so", "hmm"];

/// The correction cue that opens `text` (after fillers), if any: "actually",
/// "sorry", "wait", "I mean", "no wait", "scratch that", "or rather"… The
/// returned token is the one a faithful formatting must keep when the cue
/// sits right after a protected span.
#[must_use]
pub fn correction_cue(text: &str) -> Option<&'static str> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '⟦' && c != '⟧')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .skip_while(|w| FILLERS.contains(&w.as_str()))
        .take(2)
        .collect();
    let w0 = words.first().map(String::as_str)?;
    let w1 = words.get(1).map(String::as_str).unwrap_or("");
    match (w0, w1) {
        ("actually", _) => Some("actually"),
        ("sorry", _) => Some("sorry"),
        ("wait", _) => Some("wait"),
        ("rather", _) => Some("rather"),
        ("correction", _) => Some("correction"),
        ("scratch", "that") => Some("scratch"),
        ("i", "mean") => Some("mean"),
        ("or", "rather") => Some("rather"),
        ("no", "wait" | "sorry" | "actually" | "no") => Some("no"),
        ("no", w) if w.starts_with('⟦') || w.starts_with('/') => Some("no"),
        _ => None,
    }
}

fn follows_correction_cue(rest: &str) -> bool {
    correction_cue(rest).is_some()
}

/// Punctuation that may newly appear directly before a span.
const GLUE_OK_BEFORE: &[char] = &['(', '[', '"', '\'', '“', '‘', '`'];
/// Punctuation that may newly appear directly after a span: ordinary
/// sentence punctuation around a path or identifier is how people type.
const GLUE_OK_AFTER: &[char] = &[
    ',', '.', ';', ':', '!', '?', ')', ']', '"', '\'', '”', '’', '`',
];

/// A neighbour is "glued" when the model put something directly against the
/// span that was not there before and is not ordinary punctuation — e.g.
/// `⟦1⟧s` or `re⟦1⟧` would silently change a path or identifier.
fn glued(out: Option<char>, input: Option<char>, ok: &[char]) -> bool {
    match out {
        None => false,
        Some(c) if c.is_whitespace() || ok.contains(&c) => false,
        Some(c) => Some(c) != input,
    }
}

fn file_ext_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^[\w.-]*\w\.(rs|py|pyi|ts|tsx|js|jsx|mjs|cjs|md|toml|json|jsonl|ya?ml|sh|bash|zsh|fish|txt|go|c|h|cc|cpp|hpp|java|kt|rb|lock|sql|html|css|scss|vue|svelte|lua|nix|conf|ini|env|log|csv|xml|proto|swift|zig|ex|exs|php|pl|r|ipynb|dockerfile|mk|cmake|gradle)$",
        )
        .unwrap()
    })
}

fn classify_re() -> &'static [Regex] {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        [
            // snake_case / SCREAMING_CASE / x86_64
            r"^[A-Za-z0-9]+(_[A-Za-z0-9]+)+$",
            // camelCase with an inner capital
            r"^[a-z]+[0-9]*[A-Z][A-Za-z0-9]*$",
            // PascalCase with two humps
            r"^[A-Z][a-z0-9]+[A-Z][A-Za-z0-9]*$",
            // dotted identifiers: self.config, os.path.join (each part ≥ 2)
            r"^[A-Za-z_][A-Za-z0-9_]+(\.[A-Za-z_][A-Za-z0-9_]+)+(\(\))?$",
            // calls: foo(), print(x)
            r"^[A-Za-z_][A-Za-z0-9_.]*\([^()\s]*\)$",
            // CLI flags: --release, -v
            r"^--?[A-Za-z][A-Za-z0-9-]*(=\S+)?$",
            // versions: 1.2.3, v0.34.4
            r"^v?\d+(\.\d+){2,}$",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("static pattern"))
        .collect()
    })
}

/// Commit-hash-like tokens: 7–40 lowercase hex with at least one digit and
/// one letter (the `regex` crate has no lookahead to say that).
fn is_hash(core: &str) -> bool {
    (7..=40).contains(&core.len())
        && core.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && core.bytes().any(|b| b.is_ascii_digit())
        && core.bytes().any(|b| b.is_ascii_alphabetic())
}

fn is_technical(core: &str) -> bool {
    if core.len() < 2 {
        return false;
    }
    let bytes = core.as_bytes();
    // Slash commands and absolute paths: "/research_codebase", "/etc/hosts".
    if bytes[0] == b'/' && (bytes[1].is_ascii_alphanumeric() || bytes[1] == b'_' || bytes[1] == b'.')
    {
        return true;
    }
    // Home/relative paths and @-references ("@src/main.rs" in Claude Code).
    if core.starts_with("~/")
        || core.starts_with("./")
        || core.starts_with("../")
        || (bytes[0] == b'@' && bytes[1].is_ascii_alphanumeric())
    {
        return true;
    }
    if core.contains("://") || core.starts_with("www.") || core.contains("::") {
        return true;
    }
    // Emails.
    if let Some(at) = core.find('@') {
        if at > 0 && core[at + 1..].contains('.') {
            return true;
        }
    }
    // Interior slash: src/main.rs, feature/login. Not fractions ("1/2").
    if let Some(i) = core.find('/') {
        if i > 0
            && i + 1 < core.len()
            && !core.bytes().all(|b| b.is_ascii_digit() || b == b'/')
        {
            return true;
        }
    }
    if file_ext_re().is_match(core) || is_hash(core) {
        return true;
    }
    classify_re().iter().any(|re| re.is_match(core))
}

/// Conservative detector for tokens that are unmistakably technical: slash
/// commands, paths, URLs, emails, `@`-references, identifiers (snake_case,
/// camelCase, PascalCase, dotted, calls), CLI flags, versions, hashes, and
/// backtick code. Trailing sentence punctuation is excluded from each span.
#[must_use]
pub fn fallback_spans(text: &str) -> Vec<Range<usize>> {
    static BACKTICK: OnceLock<Regex> = OnceLock::new();
    let backtick = BACKTICK.get_or_init(|| Regex::new(r"`[^`\n]+`").unwrap());
    let mut spans: Vec<Range<usize>> = backtick.find_iter(text).map(|m| m.range()).collect();

    let mut offset = 0;
    for token in text.split_inclusive(char::is_whitespace) {
        let start = offset;
        offset += token.len();
        let token = token.trim_end();
        if token.is_empty() || spans.iter().any(|s| s.start <= start && start < s.end) {
            continue;
        }
        // Strip wrapping punctuation that is sentence, not token.
        let lead = token.len()
            - token
                .trim_start_matches(['(', '[', '"', '\'', '“', '‘'])
                .len();
        let mut core = &token[lead..];
        loop {
            let trimmed = core.trim_end_matches([',', ';', ':', '!', '?', '"', '\'', '”', '’', ']']);
            // A trailing ')' belongs to the token only for calls: "foo()".
            let trimmed = if trimmed.ends_with(')') && !trimmed.contains('(') {
                &trimmed[..trimmed.len() - 1]
            } else {
                trimmed
            };
            // A trailing '.' is a full stop unless the token is a
            // version/path/identifier that itself ends there — none do.
            let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed);
            if trimmed.len() == core.len() {
                break;
            }
            core = trimmed;
        }
        if is_technical(core) {
            let s = start + lead;
            spans.push(s..s + core.len());
        }
    }
    spans.sort_by_key(|r| r.start);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protected(text: &str) -> Vec<&str> {
        fallback_spans(text).into_iter().map(|r| &text[r]).collect()
    }

    #[test]
    fn detects_technical_tokens() {
        let t = "/research_codebase look at src/main.rs, then run cargo test --release \
                 and check self.config or parse_host_port() in ~/code/x at https://x.io/a \
                 mail a@b.io about useEffect and HashMap v1.2.3 commit a1b2c3d `let x = 1` \
                 @src/lib.rs std::fs";
        assert_eq!(
            protected(t),
            [
                "/research_codebase",
                "src/main.rs",
                "--release",
                "self.config",
                "parse_host_port()",
                "~/code/x",
                "https://x.io/a",
                "a@b.io",
                "useEffect",
                "HashMap",
                "v1.2.3",
                "a1b2c3d",
                "`let x = 1`",
                "@src/lib.rs",
                "std::fs",
            ]
        );
    }

    #[test]
    fn leaves_ordinary_prose_alone() {
        // Must-not-protect negatives: abbreviations, contractions, numbers,
        // times, money, hyphenation, fractions, acronyms, sentence ends.
        let t = "e.g. I'm sure the U.S. team won't ship 3.5 at 5:30 for $20 — a well-known \
                 1/2 step. It's OK, NASA said so. Done. (Really!) - no";
        assert!(protected(t).is_empty(), "{:?}", protected(t));
    }

    #[test]
    fn trailing_punctuation_is_not_part_of_the_span() {
        assert_eq!(protected("open README.md."), ["README.md"]);
        assert_eq!(protected("(see src/lib.rs)"), ["src/lib.rs"]);
        assert_eq!(protected("is it main.rs?"), ["main.rs"]);
    }

    #[test]
    fn normalize_rejects_bad_ranges_and_merges_overlaps() {
        let t = "abc déf ghi";
        assert!(matches!(
            normalize_spans(t, &[(5..6)], false),
            Err(SpanError::InvalidRange(_))
        ));
        assert!(matches!(
            normalize_spans(t, &[(0..99)], false),
            Err(SpanError::InvalidRange(_))
        ));
        let spans = normalize_spans(t, &[(8..11), (0..2), (1..3)], false).unwrap();
        assert_eq!(spans, [0..3, 8..11]);
        // Empty ranges are dropped, not errors.
        assert!(normalize_spans(t, &[(2..2)], false).unwrap().is_empty());
    }

    #[test]
    fn leading_slash_command_is_detached_and_never_sent() {
        let t = "/research_codebase i would like you to look at src/auth.rs today";
        let spans = normalize_spans(t, &[], true).unwrap();
        let m = mask(t, &spans, MaskStyle::Brackets).unwrap();
        assert_eq!(m.prefix, "/research_codebase ");
        assert_eq!(m.body, "i would like you to look at ⟦1⟧ today");
        assert!(!m.body.contains("research"));
        let out = m.restore("I would like you to look at ⟦1⟧ today.").unwrap();
        assert_eq!(out, "/research_codebase I would like you to look at src/auth.rs today.");
        // The pronoun is capitalized even though the dictation had "i".
    }

    #[test]
    fn continuation_after_a_detached_command_keeps_its_case() {
        let t = "/describe_pr and mention the migration";
        let m = mask(t, &normalize_spans(t, &[], true).unwrap(), MaskStyle::Brackets).unwrap();
        assert_eq!(
            m.restore("And mention the migration.").unwrap(),
            "/describe_pr and mention the migration."
        );
        let t = "/research_codebase i would like a summary";
        let m = mask(t, &normalize_spans(t, &[], true).unwrap(), MaskStyle::Brackets).unwrap();
        assert_eq!(
            m.restore("I would like a summary.").unwrap(),
            "/research_codebase I would like a summary."
        );
        // Without a prefix the model's sentence case stands.
        let t = "look at src/a.rs";
        let m = mask(t, &normalize_spans(t, &[], true).unwrap(), MaskStyle::Brackets).unwrap();
        assert_eq!(m.restore("Look at ⟦1⟧.").unwrap(), "Look at src/a.rs.");
    }

    #[test]
    fn several_leading_spans_detach_together() {
        let t = "/review src/a.rs  please check it";
        let spans = normalize_spans(t, &[], true).unwrap();
        let m = mask(t, &spans, MaskStyle::Brackets).unwrap();
        assert_eq!(m.prefix, "/review src/a.rs  ");
        assert_eq!(m.body, "please check it");
    }

    #[test]
    fn a_leading_span_glued_to_punctuation_is_masked_not_detached() {
        let t = "/commit, then push";
        let m = mask(t, &normalize_spans(t, &[], true).unwrap(), MaskStyle::Brackets).unwrap();
        assert_eq!(m.prefix, "");
        assert_eq!(m.body, "⟦1⟧, then push");
    }

    #[test]
    fn a_leading_span_being_corrected_is_masked_not_detached() {
        let t = "/create_plan uh actually no /research_codebase first";
        let m = mask(t, &normalize_spans(t, &[], true).unwrap(), MaskStyle::Brackets).unwrap();
        assert_eq!(m.prefix, "");
        assert_eq!(m.body, "⟦1⟧ uh actually no ⟦2⟧ first");
    }

    #[test]
    fn correction_cues() {
        for (t, cue) in [
            ("actually no, the other one", Some("actually")),
            ("uh sorry I meant", Some("sorry")),
            ("I mean the settings file", Some("mean")),
            ("no wait", Some("no")),
            ("no ⟦2⟧ instead", Some("no")),
            (", or rather the old one", Some("rather")),
            ("scratch that", Some("scratch")),
            ("no longer works", None),
            ("is failing", None),
            ("", None),
        ] {
            assert_eq!(correction_cue(t), cue, "{t:?}");
        }
    }

    #[test]
    fn whole_text_protected_leaves_an_empty_body() {
        let t = "/compact";
        let m = mask(t, &normalize_spans(t, &[], true).unwrap(), MaskStyle::Brackets).unwrap();
        assert_eq!(m.prefix, "/compact");
        assert_eq!(m.body, "");
    }

    fn masked(t: &str) -> Masked {
        mask(t, &normalize_spans(t, &[], true).unwrap(), MaskStyle::Brackets).unwrap()
    }

    #[test]
    fn restore_rejects_every_kind_of_span_damage() {
        let m = masked("so look at src/a.rs and src/b.rs ok");
        assert_eq!(m.body, "so look at ⟦1⟧ and ⟦2⟧ ok");
        assert_eq!(
            m.restore("So look at ⟦1⟧ and ⟦2⟧, OK.").unwrap(),
            "So look at src/a.rs and src/b.rs, OK."
        );
        assert_eq!(m.restore("Look at ⟦1⟧."), Err(SpanError::Dropped(2)));
        assert_eq!(
            m.restore("Look at ⟦1⟧ and ⟦2⟧ and ⟦2⟧."),
            Err(SpanError::Duplicated(2))
        );
        assert_eq!(m.restore("Look at ⟦2⟧ and ⟦1⟧."), Err(SpanError::Reordered));
        assert_eq!(
            m.restore("Look at ⟦1⟧ and ⟦2⟧ and ⟦3⟧."),
            Err(SpanError::Unknown(3))
        );
        assert_eq!(m.restore("Look at ⟦1⟧ and ⟦2 ok ⟦2⟧."), Err(SpanError::Debris));
        assert_eq!(m.restore("Look at ⟦1⟧s and ⟦2⟧."), Err(SpanError::Glued(1)));
        assert_eq!(m.restore("Look at the⟦1⟧ and ⟦2⟧."), Err(SpanError::Glued(1)));
    }

    #[test]
    fn glue_that_was_in_the_input_is_fine() {
        let m = masked("check (src/a.rs) now");
        assert_eq!(m.body, "check (⟦1⟧) now");
        assert_eq!(m.restore("Check (⟦1⟧) now.").unwrap(), "Check (src/a.rs) now.");
    }

    #[test]
    fn collisions_are_refused() {
        assert_eq!(
            mask("a ⟦1⟧ b", &[], MaskStyle::Brackets),
            Err(SpanError::Collision)
        );
        assert_eq!(mask("a ZQX9 b", &[], MaskStyle::Letters), Err(SpanError::Collision));
        assert!(mask("a <k1/> b", &[], MaskStyle::Brackets).is_ok());
    }

    #[test]
    fn every_style_round_trips() {
        let t = "so look at src/a.rs and src/b.rs ok";
        let spans = normalize_spans(t, &[], true).unwrap();
        for style in MaskStyle::ALL {
            let m = mask(t, &spans, style).unwrap();
            let out = m.body.replacen("so", "So", 1);
            assert_eq!(m.restore(&out).unwrap(), "So look at src/a.rs and src/b.rs ok");
            assert_eq!(MaskStyle::parse(style.as_str()), Some(style));
        }
    }

    #[test]
    fn span_text_is_restored_byte_for_byte_even_if_it_looks_like_debris() {
        // A span containing bracket characters is the span's business.
        let t = "use x";
        let m = mask(t, &[(4..5)], MaskStyle::Brackets).unwrap();
        let m = Masked {
            spans: vec!["⟦raw⟧".into()],
            ..m
        };
        assert_eq!(m.restore("Use ⟦1⟧.").unwrap(), "Use ⟦raw⟧.");
    }

    #[test]
    fn unprotected_word_count_ignores_spans() {
        let t = "/compact now please";
        let spans = normalize_spans(t, &[], true).unwrap();
        assert_eq!(unprotected_words(t, &spans), 2);
        assert_eq!(unprotected_words("/compact", &normalize_spans("/compact", &[], true).unwrap()), 0);
    }
}
