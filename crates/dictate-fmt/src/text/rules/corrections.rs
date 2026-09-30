//! `builtin_corrections`: the 23 historical Whisper mis-hearings.
//!
//! Moved here from `dictate-stt::apply_corrections`, which applied them as
//! raw substring replacements (so `" cloud"` turned "cloudy" into "claudey"
//! and "cloud.google.com" into "claude.google.com"). Here they are whole-word
//! matches that never reach inside a protected span, with three fixes:
//!
//! - word forms produce the proper noun `Claude` regardless of input case;
//! - the dot-directory forms (`.clod`, `.cloud`, `.clawed`) are corrected as
//!   **path segments**, inside detected paths (`~/.cloud/settings.json` →
//!   `~/.claude/settings.json`) — the only place this stage edits a span;
//! - the slash-command outputs (`/create_plan`, …) are protected, so no later
//!   stage or LLM can strip the slash.
//!
//! The historical table, for the record (all 23 intents are covered):
//!
//! | historical pattern | now |
//! |---|---|
//! | `.clod` `.cloud` `.clawed` | `.claude` path segment |
//! | ` clod` ` cloud` ` clawed` `Clod` `Cloud` `Clawed` | word → `Claude` |
//! | `research code base`, `research codebase` (+ capitalized) | `/research_codebase` |
//! | `create plan` (+ capitalized) | `/create_plan` |
//! | `implement plan` (+ capitalized) | `/implement_plan` |
//! | `validate plan` (+ capitalized) | `/validate_plan` |
//! | `create handoff`, `create hand off` (+ capitalized) | `/create_handoff` |

use crate::text::lex::{Editor, Kind};
use crate::text::{FormatContext, SpanKind, TextDoc, TextStage};

/// The historical acoustic corrections, word-boundary aware.
#[derive(Debug, Default, Clone, Copy)]
pub struct BuiltinCorrections;

impl TextStage for BuiltinCorrections {
    fn name(&self) -> &'static str {
        "builtin_corrections"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        let fixes: Vec<(usize, String)> = doc
            .spans()
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == SpanKind::Path)
            .filter_map(|(i, s)| fix_dot_dirs(&s.text).map(|t| (i, t)))
            .collect();
        for (i, text) in fixes {
            doc.amend_span(i, text);
        }
        doc.edit(correct_words);
    }
}

const CLAUDE_MISHEARINGS: &[&str] = &["clod", "cloud", "clawed"];
const DOT_DIR_MISHEARINGS: &[&str] = &[".clod", ".cloud", ".clawed"];

/// Spoken phrase → slash command. Longest phrase first where they share a head.
const COMMANDS: &[(&[&str], &str)] = &[
    (&["research", "code", "base"], "/research_codebase"),
    (&["research", "codebase"], "/research_codebase"),
    (&["create", "hand", "off"], "/create_handoff"),
    (&["create", "handoff"], "/create_handoff"),
    (&["create", "plan"], "/create_plan"),
    (&["implement", "plan"], "/implement_plan"),
    (&["validate", "plan"], "/validate_plan"),
];

/// `~/.cloud/settings.json` → `~/.claude/settings.json`. `None` if unchanged.
pub(crate) fn fix_dot_dirs(path: &str) -> Option<String> {
    if !DOT_DIR_MISHEARINGS
        .iter()
        .any(|m| path.to_ascii_lowercase().contains(m))
    {
        return None;
    }
    let mut changed = false;
    let fixed: Vec<&str> = path
        .split('/')
        .map(|seg| {
            if DOT_DIR_MISHEARINGS
                .iter()
                .any(|m| m.eq_ignore_ascii_case(seg))
            {
                changed = true;
                ".claude"
            } else {
                seg
            }
        })
        .collect();
    changed.then(|| fixed.join("/"))
}

/// Punctuation that makes a neighbouring word part of a larger token.
fn glue_before(ed: &Editor<'_>, i: usize) -> bool {
    ed.touching_prev(i).is_some_and(|p| match ed.kind(p) {
        Kind::Newline => false,
        Kind::Punct => matches!(
            ed.text(p),
            "-" | "." | "/" | "\\" | "@" | "#" | "$" | "~" | "_"
        ),
        _ => true,
    })
}

fn glue_after(ed: &Editor<'_>, i: usize) -> bool {
    ed.touching_next(i).is_some_and(|n| {
        if ed.kind(n) != Kind::Punct {
            return ed.kind(n) != Kind::Newline;
        }
        match ed.text(n) {
            "-" | "/" | "\\" | "@" | "_" => true,
            "." => ed
                .touching_next(n)
                .is_some_and(|m| matches!(ed.kind(m), Kind::Word | Kind::Protected)),
            _ => false,
        }
    })
}

fn correct_words(ed: &mut Editor<'_>) {
    let n = ed.len();
    let mut i = 0;
    while i < n {
        if !ed.is_word(i) {
            i += 1;
            continue;
        }
        if let Some(next) = try_command(ed, i) {
            i = next;
            continue;
        }
        try_claude(ed, i);
        i += 1;
    }
}

fn try_command(ed: &mut Editor<'_>, i: usize) -> Option<usize> {
    'phrases: for (words, command) in COMMANDS {
        if !ed.text(i).eq_ignore_ascii_case(words[0]) {
            continue;
        }
        let mut consumed = vec![i];
        let mut cur = i;
        for w in &words[1..] {
            match ed.next_word_after_space(cur) {
                Some((sp, j)) if ed.text(j).eq_ignore_ascii_case(w) => {
                    consumed.push(sp);
                    consumed.push(j);
                    cur = j;
                }
                _ => continue 'phrases,
            }
        }
        if glue_before(ed, i) || glue_after(ed, cur) {
            continue;
        }
        for &k in &consumed[1..] {
            ed.delete(k);
        }
        ed.replace_protected(i, (*command).to_string(), SpanKind::SlashCommand);
        return Some(cur + 1);
    }
    None
}

fn try_claude(ed: &mut Editor<'_>, i: usize) {
    let word = ed.text(i);
    let (stem, suffix) = match word
        .strip_suffix("'s")
        .map(|s| (s, "'s"))
        .or_else(|| word.strip_suffix("\u{2019}s").map(|s| (s, "\u{2019}s")))
    {
        Some(split) => split,
        None => (word, ""),
    };
    if !CLAUDE_MISHEARINGS
        .iter()
        .any(|m| m.eq_ignore_ascii_case(stem))
    {
        return;
    }
    if glue_before(ed, i) || glue_after(ed, i) {
        return;
    }
    let fixed = format!("Claude{suffix}");
    ed.replace(i, fixed);
}

#[cfg(test)]
mod tests {
    use crate::text::rules::test_support::{chain_with, stage_after_protect};

    use super::*;

    fn fix(input: &str) -> String {
        stage_after_protect(&BuiltinCorrections, input)
    }

    /// One assertion per historical pair: every intent keeps working.
    #[test]
    fn all_23_historical_intents_keep_working() {
        let cases: [(&str, &str); 23] = [
            ("open .clod", "open .claude"),
            ("open .cloud", "open .claude"),
            ("open .clawed", "open .claude"),
            ("ask clod about it", "ask Claude about it"),
            ("ask cloud about it", "ask Claude about it"),
            ("ask clawed about it", "ask Claude about it"),
            ("Clod said so", "Claude said so"),
            ("Cloud said so", "Claude said so"),
            ("Clawed said so", "Claude said so"),
            ("research code base for auth", "/research_codebase for auth"),
            ("research codebase for auth", "/research_codebase for auth"),
            ("then create plan", "then /create_plan"),
            ("then implement plan", "then /implement_plan"),
            ("then validate plan", "then /validate_plan"),
            ("then create handoff", "then /create_handoff"),
            ("then create hand off", "then /create_handoff"),
            ("Research code base for auth", "/research_codebase for auth"),
            ("Research codebase for auth", "/research_codebase for auth"),
            ("Create plan now", "/create_plan now"),
            ("Implement plan now", "/implement_plan now"),
            ("Validate plan now", "/validate_plan now"),
            ("Create handoff now", "/create_handoff now"),
            ("Create hand off now", "/create_handoff now"),
        ];
        for (input, want) in cases {
            assert_eq!(fix(input), want, "input: {input}");
        }
    }

    #[test]
    fn dot_dir_forms_are_fixed_inside_paths() {
        assert_eq!(
            fix("edit ~/.cloud/settings.json"),
            "edit ~/.claude/settings.json"
        );
        assert_eq!(
            fix("see ./.clod/commands/x.md"),
            "see ./.claude/commands/x.md"
        );
        assert_eq!(fix("the .clawed/agents dir"), "the .claude/agents dir");
        assert_eq!(fix("(.cloud)"), "(.claude)");
    }

    #[test]
    fn word_boundaries_hold() {
        for input in [
            "it is cloudy today",
            "the clouds rolled in",
            "a cloud-native service",
            "iCloud sync",
            "the Cloudflare dashboard",
            "visit cloud.google.com",
            "https://example.com/cloud/x",
            "the cloud_config value",
            "clods of dirt",
            "recreate plans",
            "create plans",
            "create planner",
            "we should create a plan",
            "implement planning",
            "example.cloud",
            "/srv/app.cloud/x",
        ] {
            assert_eq!(fix(input), input, "input: {input}");
        }
    }

    #[test]
    fn possessives_and_punctuation_are_kept() {
        assert_eq!(fix("the cloud's answer."), "the Claude's answer.");
        assert_eq!(fix("ask cloud, then clod."), "ask Claude, then Claude.");
        assert_eq!(fix("run create plan."), "run /create_plan.");
    }

    #[test]
    fn slash_commands_it_produces_are_protected_from_later_stages() {
        let run = chain_with(&[&BuiltinCorrections], "create plan");
        assert_eq!(run.doc.restore(), "/create_plan");
        let span = &run.doc.spans()[0];
        assert_eq!(span.kind, SpanKind::SlashCommand);
        assert_eq!(span.text, "/create_plan");
        assert_eq!(
            run.doc.verify_output("create_plan"),
            Err(crate::text::SpanViolation {
                problem: crate::text::ViolationKind::Missing,
                kind: SpanKind::SlashCommand,
                span: "/create_plan".into(),
            })
        );
    }

    #[test]
    fn already_slashed_commands_are_untouched() {
        assert_eq!(
            fix("run /research_codebase now"),
            "run /research_codebase now"
        );
    }
}
