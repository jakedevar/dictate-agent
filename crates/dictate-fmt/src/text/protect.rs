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
        let found = detect_spans(doc.working_text());
        doc.protect_ranges(&found);
    }
}

/// Find every protectable span in `text`, sorted and non-overlapping.
///
/// Ranges never include a placeholder, so this can run on a document that
/// already has spans.
#[must_use]
pub fn detect_spans(text: &str) -> Vec<(Range<usize>, SpanKind)> {
    let mut out = backtick_spans(text);
    let ticks = out.clone();
    let mut chunk_start = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if c.is_whitespace() {
            if let Some(s) = chunk_start.take() {
                let overlaps_tick = ticks.iter().any(|(r, _)| r.start < i && s < r.end);
                if !overlaps_tick {
                    if let Some(found) = classify_chunk(text, s, i) {
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

/// Pair runs of backticks of equal length on one line: `` `x` ``, ``` ``a b`` ```.
fn backtick_spans(text: &str) -> Vec<(Range<usize>, SpanKind)> {
    if !text.contains('`') {
        return Vec::new();
    }
    let mut runs: Vec<(usize, usize)> = Vec::new(); // (start, end) byte offsets
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let s = i;
            while i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            runs.push((s, i));
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    let mut k = 0;
    while k < runs.len() {
        let (s, e) = runs[k];
        let len = e - s;
        let close = (k + 1..runs.len()).find(|&j| runs[j].1 - runs[j].0 == len);
        match close {
            Some(j) => {
                let inner = &text[e..runs[j].0];
                if !inner.trim().is_empty()
                    && !inner.contains('\n')
                    && !inner.chars().any(is_placeholder)
                {
                    out.push((s..runs[j].1, SpanKind::Code));
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

const OPENERS: &[char] = &['(', '[', '{', '<', '"', '\'', '\u{201C}', '\u{2018}'];
const TRAILERS: &[char] = &[
    '.', ',', ';', ':', '!', '?', '"', '\'', '\u{201D}', '\u{2019}', '>', '\u{2026}',
];

fn classify_chunk(text: &str, start: usize, end: usize) -> Option<(Range<usize>, SpanKind)> {
    let chunk = &text[start..end];
    if chunk.chars().any(is_placeholder) {
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
    loop {
        let core = &text[s..e];
        let c = core.chars().next_back()?;
        let strip = TRAILERS.contains(&c)
            || match c {
                ')' => core.matches('(').count() < core.matches(')').count(),
                ']' => core.matches('[').count() < core.matches(']').count(),
                '}' => core.matches('{').count() < core.matches('}').count(),
                _ => false,
            };
        if strip {
            e -= c.len_utf8();
        } else {
            break;
        }
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
    s.len() > 4 && s[..4].eq_ignore_ascii_case("www.") && is_domain(host_of(s))
}

/// Common top-level domains for scheme-less hosts. Bounded on purpose: a bare
/// `word.word` is prose far more often than a URL.
const TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "ai", "app", "co", "gov", "edu", "us", "uk", "ca", "de",
    "fr", "jp", "au", "eu", "info", "me", "sh", "tv", "xyz", "tech", "cloud", "so", "gg", "ly",
    "fm", "page", "site", "blog", "news", "xxx", "biz", "ru", "cn", "in", "nl", "se", "ch", "es",
    "it", "br", "mx", "nz", "ie", "be", "at", "dk", "no", "fi", "pl", "kr", "tw", "sg", "hk", "is",
    "to", "ws", "cc", "ms", "run", "social", "chat", "codes",
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
