//! Daemon-supplied text, made safe for a terminal.
//!
//! A focused window's `WM_CLASS`, a dictated transcript, a dictionary phrase
//! or an error message can all carry bytes a terminal *interprets* — an `ESC`
//! starts a control sequence that can retitle the window, move the cursor, or
//! rewrite what the user sees. Every human-readable render of such text goes
//! through [`inline`] (one line) or [`block`] (a transcript, where newlines
//! and tabs are real content). `--json` output is serde-escaped already and
//! is left alone.

use std::borrow::Cow;

/// `s` with every control character (C0, DEL and C1 — U+009B is a one-byte
/// CSI) shown as a visible escape (`\x1b`, `\u{9b}`), newlines and tabs
/// included, so the text is guaranteed to stay on its line.
pub fn inline(s: &str) -> Cow<'_, str> {
    escape(s, false)
}

/// Like [`inline`], but `\n` and `\t` pass through: a multi-line transcript
/// stays multi-line, while escape sequences are still defused.
pub fn block(s: &str) -> Cow<'_, str> {
    escape(s, true)
}

fn escape(s: &str, keep_layout: bool) -> Cow<'_, str> {
    let unsafe_char = |c: char| c.is_control() && !(keep_layout && matches!(c, '\n' | '\t'));
    if !s.chars().any(unsafe_char) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if unsafe_char(c) {
            match u8::try_from(u32::from(c)) {
                Ok(b) if c.is_ascii() => out.push_str(&format!("\\x{b:02x}")),
                _ => out.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
            }
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_is_untouched_and_unallocated() {
        assert!(matches!(inline("Café — ok"), Cow::Borrowed(_)));
        assert!(matches!(block("a\nb\tc"), Cow::Borrowed(_)));
    }

    #[test]
    fn escape_sequences_become_visible_text() {
        assert_eq!(
            inline("a\x1b]0;pwned\x07b\x1b[2J"),
            "a\\x1b]0;pwned\\x07b\\x1b[2J"
        );
        assert_eq!(inline("x\u{9b}31m"), "x\\u{9b}31m");
        assert_eq!(inline("x\x7fy"), "x\\x7fy");
    }

    #[test]
    fn inline_flattens_layout_and_block_keeps_it() {
        assert_eq!(inline("a\nb\tc\r"), "a\\x0ab\\x09c\\x0d");
        assert_eq!(block("a\nb\tc\x1b[0m"), "a\nb\tc\\x1b[0m");
    }
}
