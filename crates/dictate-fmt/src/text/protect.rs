//! Detectors for text no later stage may alter.
//!
//! Detection is token-based: the working text is split on whitespace, each
//! chunk is trimmed of surrounding sentence punctuation and brackets, and the
//! remaining core is classified. Backticked text is found first and may span
//! whitespace. The protected range is the core only, so `(see ~/.claude).`
//! protects `~/.claude` and leaves `(see`, `)` and `.` to the rules.

use std::ops::Range;

use super::doc::{is_placeholder, SpanKind, TextDoc};
use super::{FormatContext, TextStage};

/// The `protect` stage: detect and protect every built-in span kind.
#[derive(Debug, Default, Clone, Copy)]
pub struct Protect;

impl TextStage for Protect {
    fn name(&self) -> &'static str {
        "protect"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        let found = detect_spans_in(doc);
        doc.protect_ranges(&found);
    }
}

/// Find every protectable span in `text`, sorted and non-overlapping.
///
/// Ranges never include a placeholder, so this can run on a document that
/// already has spans.
#[must_use]
pub fn detect_spans(text: &str) -> Vec<(Range<usize>, SpanKind)> {
    detect(text, &is_placeholder)
}

/// [`detect_spans`] over a document's working text, reading *through* the
/// placeholders of escaped private-use characters: a literal inside a URL,
/// path or code span is part of that span (which then absorbs it), never a
/// hole that leaves the rest of the token unprotected.
pub(crate) fn detect_spans_in(doc: &TextDoc) -> Vec<(Range<usize>, SpanKind)> {
    detect(doc.working_text(), &|c| {
        is_placeholder(c) && !doc.is_literal_placeholder(c)
    })
}

/// Detection, with `opaque` naming the characters no span may contain.
fn detect(text: &str, opaque: &dyn Fn(char) -> bool) -> Vec<(Range<usize>, SpanKind)> {
    let mut out = backtick_spans(text, opaque);
    let ticks = out.clone();
    // Chunks and tick spans both arrive in order, so one cursor suffices.
    let mut tick = 0;
    let mut chunk_start = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if c.is_whitespace() {
            if let Some(s) = chunk_start.take() {
                while tick < ticks.len() && ticks[tick].0.end <= s {
                    tick += 1;
                }
                let overlaps_tick = ticks.get(tick).is_some_and(|(r, _)| r.start < i);
                if !overlaps_tick {
                    if let Some(found) = classify_chunk(text, s, i, opaque) {
                        out.push(found);
                    }
                }
            }
        } else if chunk_start.is_none() {
            chunk_start = Some(i);
        }
    }
    out.sort_by_key(|(r, _)| r.start);
    out
}

/// One run of backticks, with what precedes its two ends.
struct TickRun {
    start: usize,
    end: usize,
    /// Line breaks, non-whitespace characters and opaque characters before
    /// `start` and before `end`: an inner text's counts are differences.
    at_start: [usize; 3],
    at_end: [usize; 3],
}

/// Pair runs of backticks of equal length on one line: `` `x` ``, ``` ``a b`` ```.
///
/// Linear: each run's closer (the next run of the same length) is found by
/// one backward pass, and an inner text's emptiness, line breaks and opaque
/// characters are prefix-count differences — so unmatched runs never rescan
/// the rest of the text.
fn backtick_spans(text: &str, opaque: &dyn Fn(char) -> bool) -> Vec<(Range<usize>, SpanKind)> {
    if !text.contains('`') {
        return Vec::new();
    }
    let mut runs: Vec<TickRun> = Vec::new();
    let mut counts = [0usize; 3]; // newlines, non-whitespace, opaque
    let mut open: Option<(usize, [usize; 3])> = None;
    for (i, c) in text.char_indices() {
        if c == '`' {
            if open.is_none() {
                open = Some((i, counts));
            }
        } else if let Some((start, at_start)) = open.take() {
            runs.push(TickRun {
                start,
                end: i,
                at_start,
                at_end: counts,
            });
        }
        counts[0] += usize::from(c == '\n');
        counts[1] += usize::from(!c.is_whitespace());
        counts[2] += usize::from(opaque(c));
    }
    if let Some((start, at_start)) = open {
        runs.push(TickRun {
            start,
            end: text.len(),
            at_start,
            at_end: counts,
        });
    }
    // The next run of the same length, for every run.
    let mut closer = vec![None; runs.len()];
    let mut latest: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for k in (0..runs.len()).rev() {
        let len = runs[k].end - runs[k].start;
        closer[k] = latest.insert(len, k);
    }
    let mut out = Vec::new();
    let mut k = 0;
    while k < runs.len() {
        match closer[k] {
            Some(j) => {
                let (a, b) = (runs[k].at_end, runs[j].at_start);
                // The inner text runs from the opener's end to the closer's
                // start; the opener's own backticks are not in it.
                let one_line = a[0] == b[0];
                let not_blank = b[1] > a[1];
                let clear = a[2] == b[2];
                if one_line && not_blank && clear {
                    out.push((runs[k].start..runs[j].end, SpanKind::Code));
                    k = j + 1;
                } else {
                    k += 1;
                }
            }
            None => k += 1,
        }
    }
    out
}

/// Whisper's non-speech markers. Never protected (outside backticks), so the
/// hallucination scrub can remove them; it re-runs detection afterwards.
pub(crate) const ARTIFACT_MARKERS: &[&str] = &["[BLANK_AUDIO]"];

pub(crate) const OPENERS: &[char] = &['(', '[', '{', '<', '"', '\'', '\u{201C}', '\u{2018}'];
const TRAILERS: &[char] = &[
    '.', ',', ';', ':', '!', '?', '"', '\'', '\u{201D}', '\u{2019}', '>', '\u{2026}',
];

/// Punctuation that may follow a span without extending it: sentence
/// punctuation, closing quotes and closing brackets.
pub(crate) fn is_closer(c: char) -> bool {
    TRAILERS.contains(&c) || matches!(c, ')' | ']' | '}')
}

fn classify_chunk(
    text: &str,
    start: usize,
    end: usize,
    opaque: &dyn Fn(char) -> bool,
) -> Option<(Range<usize>, SpanKind)> {
    let chunk = &text[start..end];
    if chunk.chars().any(opaque) || ARTIFACT_MARKERS.iter().any(|m| chunk.contains(m)) {
        return None;
    }
    let mut s = start;
    let mut e = end;
    while let Some(c) = text[s..e].chars().next() {
        if OPENERS.contains(&c) {
            s += c.len_utf8();
        } else {
            break;
        }
    }
    // Bracket balance of the core, kept current as closers are stripped, so
    // a chunk of ten thousand `)` costs linear time, not quadratic.
    let count = |open: char, close: char| {
        let core = &text[s..e];
        core.matches(open).count() as isize - core.matches(close).count() as isize
    };
    let mut balance = [count('(', ')'), count('[', ']'), count('{', '}')];
    loop {
        let c = text[s..e].chars().next_back()?;
        let bracket = match c {
            ')' => Some(0),
            ']' => Some(1),
            '}' => Some(2),
            _ => None,
        };
        let strip = TRAILERS.contains(&c) || bracket.is_some_and(|b| balance[b] < 0);
        if !strip {
            break;
        }
        if let Some(b) = bracket {
            balance[b] += 1;
        }
        e -= c.len_utf8();
    }
    if s >= e {
        return None;
    }
    let core = &text[s..e];
    if let Some(kind) = classify(core) {
        return Some((s..e, kind));
    }
    // `user_id's` protects `user_id`.
    for suffix in ["'s", "\u{2019}s"] {
        if let Some(stem) = core.strip_suffix(suffix) {
            if let Some(kind) = classify(stem) {
                return Some((s..s + stem.len(), kind));
            }
        }
    }
    // A private-use literal glued to either end of a token (only literals
    // can be placeholders here; `opaque` refused the rest) must not hide
    // the token from its detector: protect the token and the literals.
    let inner = core.trim_matches(is_placeholder);
    if !inner.is_empty() && inner.len() < core.len() {
        if let Some(kind) = classify(inner) {
            return Some((s..e, kind));
        }
    }
    None
}

/// Classify a trimmed token. `None` means ordinary prose.
#[must_use]
pub(crate) fn classify(core: &str) -> Option<SpanKind> {
    if core.len() < 2 {
        return None;
    }
    if is_email(core) {
        return Some(SpanKind::Email);
    }
    if is_scheme_url(core) || is_www(core) {
        return Some(SpanKind::Url);
    }
    if is_path(core) || is_dotfile(core) {
        return Some(SpanKind::Path);
    }
    if is_slash_command(core) {
        return Some(SpanKind::SlashCommand);
    }
    if is_mention(core) {
        return Some(SpanKind::Mention);
    }
    if is_env_or_flag(core) || is_filename(core) {
        return Some(SpanKind::Code);
    }
    if is_bare_domain(core) {
        return Some(SpanKind::Url);
    }
    if is_code_identifier(core) {
        return Some(SpanKind::Code);
    }
    None
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(is_ident_start) && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_domain(d: &str) -> bool {
    let labels: Vec<&str> = d.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let tld = labels[labels.len() - 1];
    labels.iter().all(|l| {
        !l.is_empty()
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    }) && tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic())
}

fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c))
        && is_domain(domain)
}

fn is_scheme_url(s: &str) -> bool {
    let Some(idx) = s.find("://") else {
        return false;
    };
    let scheme = &s[..idx];
    scheme.len() >= 2
        && scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        && s.len() > idx + 3
}

fn host_of(s: &str) -> &str {
    let end = s.find(['/', '?', '#', ':']).unwrap_or(s.len());
    &s[..end]
}

fn is_www(s: &str) -> bool {
    // `get` rather than `[..4]`: byte 4 can fall inside a multi-byte
    // character ("café:"), and slicing there would panic the session.
    s.len() > 4
        && s.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("www."))
        && is_domain(host_of(s))
}

/// Common top-level domains for scheme-less hosts. Bounded on purpose: a bare
/// `word.word` is prose far more often than a URL.
const TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "ai", "app", "co", "gov", "edu", "us", "uk", "ca", "de",
    "fr", "jp", "au", "eu", "info", "me", "sh", "tv", "xyz", "tech", "cloud", "so", "gg", "ly",
    "fm", "page", "site", "blog", "news", "xxx", "biz", "ru", "cn", "in", "nl", "se", "ch", "es",
    "it", "br", "mx", "nz", "ie", "be", "at", "dk", "no", "fi", "pl", "kr", "tw", "sg", "hk", "is",
    "to", "ws", "cc", "ms", "run", "social", "chat", "codes", "rs",
];

fn is_bare_domain(s: &str) -> bool {
    let host = host_of(s);
    if !is_domain(host) {
        return false;
    }
    let tld = host.rsplit('.').next().unwrap_or("");
    let first = host.split('.').next().unwrap_or("");
    TLDS.iter().any(|t| t.eq_ignore_ascii_case(tld))
        && first.chars().any(|c| c.is_ascii_alphabetic())
        && host
            .split('.')
            .all(|l| l.len() >= 2 || l.chars().all(|c| c.is_ascii_digit()))
}

/// File extensions that make `name.ext` code rather than prose.
const EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "json", "jsonl", "yaml", "yml", "toml",
    "md", "mdx", "txt", "sh", "bash", "zsh", "fish", "go", "c", "h", "cc", "cpp", "hpp", "java",
    "kt", "swift", "rb", "lua", "sql", "html", "htm", "css", "scss", "lock", "log", "env", "cfg",
    "conf", "ini", "xml", "csv", "tsv", "pdf", "png", "jpg", "jpeg", "gif", "svg", "webp", "wav",
    "mp3", "mp4", "zip", "tar", "gz", "tgz", "bin", "gguf", "db", "sqlite", "service", "nix",
    "proto", "graphql", "vue", "svelte", "php", "ex", "exs", "hs", "dart", "zig", "tf", "patch",
    "diff", "ipynb", "wasm", "so", "dll", "exe", "deb", "rpm", "bak", "tmp", "orig",
];

fn has_extension(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
        && stem
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(ext))
}

fn is_filename(s: &str) -> bool {
    has_extension(s)
}

fn is_path(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("~/") {
        return !rest.is_empty();
    }
    for prefix in ["./", "../"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return !rest.is_empty();
        }
    }
    if let Some(rest) = s.strip_prefix('/') {
        // `/abs/path` — two segments, or one plus a trailing slash.
        return rest.find('/').is_some_and(|i| i > 0);
    }
    if let Some(rest) = s.strip_prefix('.') {
        // `.dotdir/…`
        return rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && rest.contains('/');
    }
    // `dir/sub/file.ext`
    if s.contains('/') {
        let segments: Vec<&str> = s.split('/').collect();
        let last = segments[segments.len() - 1];
        return segments[..segments.len() - 1]
            .iter()
            .all(|seg| !seg.is_empty())
            && has_extension(last);
    }
    false
}

fn is_dotfile(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('.') else {
        return false;
    };
    rest.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

fn is_slash_command(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('/') else {
        return false;
    };
    rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_:-".contains(c))
}

fn is_mention(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('@').or_else(|| s.strip_prefix('#')) else {
        return false;
    };
    rest.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

fn is_env_or_flag(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix('$') {
        return rest
            .chars()
            .next()
            .is_some_and(|c| is_ident_start(c) || c == '{');
    }
    if let Some(rest) = s.strip_prefix("--") {
        return rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    }
    false
}

fn is_code_identifier(s: &str) -> bool {
    // Optional trailing call: `foo(…)`, `a.b()`.
    let (path, called) = match s.find('(') {
        Some(open) if s.ends_with(')') && open > 0 => (&s[..open], true),
        Some(_) => return false,
        None => (s, false),
    };
    let (segments, sep): (Vec<&str>, &str) = if path.contains("::") {
        (path.split("::").collect(), "::")
    } else {
        (path.split('.').collect(), ".")
    };
    if !segments.iter().all(|seg| is_ident(seg)) {
        return false;
    }
    if called || sep == "::" && segments.len() > 1 {
        return true;
    }
    if segments.len() > 1 {
        // `self.config`, `os.path.join` — but not `e.g`, `i.e`, `a.m`.
        return segments.iter().all(|seg| seg.len() >= 2);
    }
    let word = segments[0];
    let snake = word.contains('_') && word.chars().any(|c| c.is_ascii_alphanumeric());
    let camel = word
        .as_bytes()
        .windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
    snake || camel
}

#[cfg(test)]
mod tests {
    #[test]
    fn multibyte_tokens_never_panic_the_detectors() {
        // Byte 4 of "café:" is inside 'é'; every detector must slice on
        // character boundaries. Mixed scripts and emoji around real spans.
        for input in [
            "café: meet at the café",
            "naïve résumé 日本語 ok",
            "ship it 🚀🚀 then run /create_plan",
            "wwwé.example.com and www.example.com",
            "éééé.com",
        ] {
            for (range, _) in detect_spans(input) {
                assert!(input.is_char_boundary(range.start), "{input:?}");
                assert!(input.is_char_boundary(range.end), "{input:?}");
            }
        }
    }

    use super::*;

    fn spans(text: &str) -> Vec<(&str, SpanKind)> {
        detect_spans(text)
            .into_iter()
            .map(|(r, k)| (&text[r], k))
            .collect()
    }

    #[test]
    fn detects_every_required_kind() {
        use SpanKind::*;
        let cases: &[(&str, &str, SpanKind)] = &[
            (
                "see https://example.com/a?b=1 now",
                "https://example.com/a?b=1",
                Url,
            ),
            ("visit www.example.org today", "www.example.org", Url),
            ("check docs.rs for it", "docs.rs", Code),
            ("go to example.com.", "example.com", Url),
            (
                "mail jane.doe@example.com today",
                "jane.doe@example.com",
                Email,
            ),
            (
                "open ~/.claude/settings.json",
                "~/.claude/settings.json",
                Path,
            ),
            ("run ./scripts/run.sh", "./scripts/run.sh", Path),
            ("up ../foo please", "../foo", Path),
            ("look in /usr/local/bin", "/usr/local/bin", Path),
            ("edit .claude/commands/x.md", ".claude/commands/x.md", Path),
            ("the .env file", ".env", Path),
            ("see src/main.rs", "src/main.rs", Path),
            (
                "run /research_codebase now",
                "/research_codebase",
                SlashCommand,
            ),
            ("then /clear", "/clear", SlashCommand),
            ("the user_id field", "user_id", Code),
            ("call getUserName", "getUserName", Code),
            ("a HashMap here", "HashMap", Code),
            ("call console.log() here", "console.log()", Code),
            ("use self.config later", "self.config", Code),
            ("use std::fs here", "std::fs", Code),
            (
                "wrap `cargo test --workspace` now",
                "`cargo test --workspace`",
                Code,
            ),
            ("ping @jake", "@jake", Mention),
            ("post in #general", "#general", Mention),
            ("echo $HOME", "$HOME", Code),
            ("pass --release", "--release", Code),
            ("edit Cargo.toml", "Cargo.toml", Code),
        ];
        for (text, want, kind) in cases {
            let got = spans(text);
            assert_eq!(got, vec![(*want, *kind)], "input: {text}");
        }
    }

    #[test]
    fn trims_sentence_punctuation_and_brackets_but_keeps_balanced_parens() {
        assert_eq!(
            spans("(see ~/.claude/x). Then foo.bar(), \"user_id\"."),
            vec![
                ("~/.claude/x", SpanKind::Path),
                ("foo.bar()", SpanKind::Code),
                ("user_id", SpanKind::Code),
            ]
        );
        assert_eq!(
            spans("the user_id's value"),
            vec![("user_id", SpanKind::Code)],
            "possessive stays prose"
        );
    }

    #[test]
    fn leaves_prose_alone() {
        for text in [
            "Hello there, how are you?",
            "e.g. this and i.e. that at 5 p.m.",
            "and/or either/or 24/7 TCP/IP",
            "a well-known state-of-the-art thing",
            "The U.S. economy grew 3.5% to $5,000.",
            "Wait... what? No.",
            "I'm here, it's fine, don't worry.",
            "version 2.1.3 of the API is out",
            "C# and C++ are languages",
            "an / alone and a // comment",
            "Mr. Smith met Dr. Jones.",
        ] {
            assert_eq!(spans(text), vec![], "input: {text}");
        }
    }

    #[test]
    fn single_segment_absolute_paths_are_slash_commands_not_paths() {
        assert_eq!(spans("/tmp"), vec![("/tmp", SpanKind::SlashCommand)]);
        assert_eq!(spans("/tmp/x"), vec![("/tmp/x", SpanKind::Path)]);
        assert_eq!(spans("/etc/"), vec![("/etc/", SpanKind::Path)]);
    }

    #[test]
    fn backticks_pair_by_run_length_and_stay_on_one_line() {
        assert_eq!(
            spans("run ``a `b` c`` then `d`"),
            vec![("``a `b` c``", SpanKind::Code), ("`d`", SpanKind::Code)]
        );
        assert_eq!(spans("a ` b\nc ` d"), vec![], "no pairing across lines");
        assert_eq!(spans("lonely ` tick"), vec![]);
    }

    #[test]
    fn existing_placeholders_are_never_inside_a_new_span() {
        let text = "x \u{F0000} user_id";
        assert_eq!(spans(text), vec![("user_id", SpanKind::Code)]);
    }
}
