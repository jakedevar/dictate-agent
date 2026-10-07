//! `hallucination_scrub`: Whisper/LLM artifacts and whitespace.

use crate::text::protect::{detect_spans_in, ARTIFACT_MARKERS};
use crate::text::{FormatContext, SpanKind, TextDoc, TextStage};

/// Removes known non-speech artifacts and normalizes whitespace.
///
/// Runs after `protect`, on unprotected text only: every protected span is a
/// single opaque placeholder here, so its bytes — the two spaces in
/// `` `a  b` ``, a tab in code, a zero-width character in a URL — are never
/// touched.
///
/// - `[BLANK_AUDIO]`, whisper.cpp's non-speech marker, anywhere outside
///   backticks (detection never protects it there).
/// - A trailing `/no_think` (a Qwen3 directive that leaked into output) —
///   protected as a slash command, and removed anyway: the one protected span
///   this stage may drop, and only at the very end.
/// - A trailing `Thank you.` **only when it is its own sentence** — Whisper's
///   classic hallucination on trailing silence. "I wanted to thank you." and
///   "Thank you for the review." are left alone.
/// - Whitespace: CRLF → LF, tabs and exotic spaces → one space, zero-width
///   characters dropped, runs collapsed, spaces around line breaks and at
///   both ends trimmed.
///
/// If it changed anything it re-runs detection, so a token it separated from
/// an artifact (`https://x.example/a[BLANK_AUDIO]`) is protected too.
#[derive(Debug, Default, Clone, Copy)]
pub struct HallucinationScrub;

impl TextStage for HallucinationScrub {
    fn name(&self) -> &'static str {
        "hallucination_scrub"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        let artifacts: Vec<char> = doc
            .spans()
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == SpanKind::SlashCommand && is_no_think(&s.text))
            .map(|(i, _)| crate::text::doc::placeholder_for(i))
            .collect();
        let mut changed = false;
        doc.rewrite_unprotected(|src, out| {
            scrub(src, &|c| artifacts.contains(&c), out);
            changed = out != src;
        });
        if changed {
            let found = detect_spans_in(doc);
            doc.protect_ranges(&found);
        }
    }
}

fn is_no_think(s: &str) -> bool {
    s.eq_ignore_ascii_case("/no_think")
}

fn scrub(src: &str, is_artifact: &dyn Fn(char) -> bool, out: &mut String) {
    if ARTIFACT_MARKERS.iter().any(|m| src.contains(m)) {
        let mut stripped = src.to_string();
        for m in ARTIFACT_MARKERS {
            stripped = stripped.replace(m, " ");
        }
        normalize_whitespace(&stripped, out);
    } else {
        normalize_whitespace(src, out);
    }
    strip_trailing_artifacts(out, is_artifact);
}

fn normalize_whitespace(src: &str, out: &mut String) {
    out.clear();
    out.reserve(src.len());
    let mut pending_space = false;
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{200B}' | '\u{FEFF}' | '\u{2060}' => {}
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    push_newline(out, &mut pending_space);
                }
            }
            '\n' => push_newline(out, &mut pending_space),
            c if c.is_whitespace() => pending_space = true,
            c => {
                if pending_space && !out.is_empty() && !out.ends_with('\n') {
                    out.push(' ');
                }
                pending_space = false;
                out.push(c);
            }
        }
    }
    while out.ends_with(['\n', ' ']) {
        out.pop();
    }
}

fn push_newline(out: &mut String, pending_space: &mut bool) {
    *pending_space = false;
    if !out.is_empty() {
        out.push('\n');
    }
}

fn strip_trailing_artifacts(text: &mut String, is_artifact: &dyn Fn(char) -> bool) {
    // `/no_think`, as its protected placeholder or (in a document where it
    // was not protected) as text: the start of it at the end of `t`.
    let no_think_at = |t: &str| -> Option<usize> {
        if let Some(c) = t.chars().next_back().filter(|c| is_artifact(*c)) {
            return Some(t.len() - c.len_utf8());
        }
        ends_with_ci(t, "/no_think")
    };
    let mut stripped_any = false;
    loop {
        let t = text.trim_end();
        let cut = if let Some(start) = no_think_at(t) {
            let before = t[..start].chars().next_back();
            (before.is_none() || before.is_some_and(char::is_whitespace)).then_some(start)
        } else if let Some(start) = ends_with_ci(t, "thank you.") {
            let head = t[..start].trim_end();
            let own_sentence = head.is_empty()
                || head.ends_with(['.', '!', '?', '\n'])
                || t[..start].ends_with('\n')
                || no_think_at(head).is_some();
            let starts_word = t[..start]
                .chars()
                .next_back()
                .is_none_or(|c| c.is_whitespace());
            (own_sentence && starts_word).then_some(start)
        } else {
            None
        };
        let Some(start) = cut else { break };
        text.truncate(start);
        stripped_any = true;
    }
    if stripped_any {
        // "Thanks, thank you." never leaves a dangling separator.
        while text.ends_with([' ', '\n', ',', ';', ':']) {
            text.pop();
        }
    }
}

fn ends_with_ci(text: &str, suffix: &str) -> Option<usize> {
    let start = text.len().checked_sub(suffix.len())?;
    (text.is_char_boundary(start) && text[start..].eq_ignore_ascii_case(suffix)).then_some(start)
}

#[cfg(test)]
mod tests {
    use crate::text::rules::test_support::stage_after_protect as stage;

    use super::*;

    #[test]
    fn scrubs_artifacts_and_normalizes_whitespace() {
        let cases: &[(&str, &str)] = &[
            ("Fix the bug. Thank you.", "Fix the bug."),
            ("Fix the bug.  THANK YOU.", "Fix the bug."),
            ("Thank you.", ""),
            ("Thank you. Thank you.", ""),
            ("Ship it /no_think", "Ship it"),
            ("Ship it. /NO_THINK Thank you.", "Ship it."),
            ("Ship it, /no_think", "Ship it"),
            ("[BLANK_AUDIO]", ""),
            ("[BLANK_AUDIO] hello [BLANK_AUDIO] there", "hello there"),
            ("  hello \t  world  ", "hello world"),
            ("line one \r\n  line two", "line one\nline two"),
            ("a\u{00A0}b\u{2003}c", "a b c"),
            ("zero\u{200B}width", "zerowidth"),
            ("First.\nThank you.", "First."),
        ];
        for (input, want) in cases {
            assert_eq!(stage(&HallucinationScrub, input), *want, "input: {input:?}");
        }
    }

    #[test]
    fn never_touches_a_legitimate_thank_you() {
        for input in [
            "Thank you for the quick review.",
            "I just wanted to thank you.",
            "Please thank you know who.",
            "thank you",
            "Say thank you. Then leave.",
            "No thanks, thank youuu.",
            "run /no_think_mode now",
            "the path a/no_think",
        ] {
            assert_eq!(stage(&HallucinationScrub, input), input, "input: {input:?}");
        }
    }

    /// `SCRUB_CORRUPTS_PROTECTED_BYTES`: the scrub runs on unprotected text
    /// only, so code, URLs and paths keep their exact bytes.
    #[test]
    fn the_scrub_never_changes_protected_bytes() {
        let chain = crate::text::TextChain::default();
        let ctx = crate::text::FormatContext::default();
        for input in [
            "Keep `a  b` exactly.",
            "Keep `[BLANK_AUDIO]` exactly.",
            "Keep `a\tb` exactly.",
            "Open https://example.com/a\u{200B}b now.",
            "Open ~/x/\u{FEFF}y.rs now.",
            "Keep `say thank you.` and `/no_think` here.",
        ] {
            assert_eq!(chain.format(input, &ctx), input, "input: {input:?}");
        }
        // The artifacts it exists for are still removed.
        for (input, want) in [
            ("Ship it /no_think", "Ship it"),
            ("Ship it. /NO_THINK Thank you.", "Ship it."),
            ("[BLANK_AUDIO] hello [BLANK_AUDIO] there", "Hello there"),
            (
                "open https://example.com/x[BLANK_AUDIO]",
                "Open https://example.com/x",
            ),
            ("fix  the\u{200B} bug. Thank you.", "Fix the bug."),
        ] {
            assert_eq!(chain.format(input, &ctx), want, "input: {input:?}");
        }
    }
}
