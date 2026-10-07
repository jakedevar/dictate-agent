#[derive(Debug, Clone, PartialEq)]
pub enum RouteType {
    Type,
    Local,
    Timer,
    Edit,
    #[allow(dead_code)]
    Command,
    /// Append to the scratchpad instead of typing (S35).
    Note,
}

#[derive(Debug, Clone)]
pub struct RouteResult {
    pub route: RouteType,
    pub model: String,
    pub text: String,
    pub confidence: f64,
}

const EDIT_TRIGGERS: &[&str] = &["edit:", "fix:", "change:", "rewrite:", "transform:"];
const LOCAL_TRIGGERS: &[&str] = &["simple", "easy", "medium", "hard"];

/// Spoken openers for the scratchpad, as whole words.
///
/// A bare "note" is deliberately **not** one: "note that the deadline moved"
/// is ordinary prose, and routing it would take the sentence away from the
/// window the user was typing into. "note:" and "note," (what Whisper writes
/// for a deliberate "note, …") are, as are the unambiguous phrases below.
const NOTE_PHRASES: &[&[&str]] = &[
    &["note", "to", "self"],
    &["quick", "note"],
    &["new", "note"],
    &["take", "a", "note"],
    &["make", "a", "note"],
    &["add", "a", "note"],
];

/// If `text` opens with a scratchpad trigger, the note body that follows it.
fn note_body(text: &str) -> Option<&str> {
    let is_sep =
        |c: char| c.is_whitespace() || matches!(c, ':' | ',' | ';' | '.' | '-' | '–' | '—');
    // "note:" / "note," (also unspaced, "note:buy milk").
    for opener in ["note:", "note,"] {
        if text
            .get(..opener.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(opener))
        {
            return Some(text[opener.len()..].trim_start_matches(is_sep).trim_end());
        }
    }
    // Multi-word phrases, matched as whole words and allowing punctuation
    // after the last one ("Note to self: …", "Quick note, …").
    'phrase: for phrase in NOTE_PHRASES {
        let mut rest = text;
        for (i, want) in phrase.iter().enumerate() {
            rest = rest.trim_start();
            let end = rest
                .find(|c: char| !c.is_alphabetic())
                .unwrap_or(rest.len());
            if !rest[..end].eq_ignore_ascii_case(want) {
                continue 'phrase;
            }
            rest = &rest[end..];
            // Words inside the phrase are separated by spaces; the last one
            // must end at a boundary ("quick notebook" is not a trigger).
            let boundary = if i + 1 < phrase.len() {
                rest.starts_with(char::is_whitespace)
            } else {
                rest.chars().next().is_none_or(|c| !c.is_alphanumeric())
            };
            if !boundary {
                continue 'phrase;
            }
        }
        return Some(rest.trim_start_matches(is_sep).trim_end());
    }
    None
}

/// Route transcribed text by keyword prefix.
/// Direct port of dictate/router.py:43-79.
///
/// Priority:
/// 1. Empty text → Type
/// 2. Edit triggers (colon-suffixed, or spoken "edit") → Edit
/// 3. Scratchpad triggers ("note:", "note to self", "quick note", …) → Note
/// 4. "timer" prefix → Timer
/// 5. LOCAL_TRIGGERS prefix → Local
/// 6. Default → Type (with original text preserved)
pub fn route(text: &str) -> RouteResult {
    let text = text.trim();
    if text.is_empty() {
        return RouteResult {
            route: RouteType::Type,
            model: String::new(),
            text: text.into(),
            confidence: 1.0,
        };
    }

    // Check edit triggers (colon-suffixed prefixes)
    let lower = text.to_lowercase();
    for trigger in EDIT_TRIGGERS {
        if lower.starts_with(trigger) {
            return RouteResult {
                route: RouteType::Edit,
                model: "local".into(),
                text: text[trigger.len()..].trim().into(),
                confidence: 1.0,
            };
        }
    }

    if let Some(body) = note_body(text) {
        return RouteResult {
            route: RouteType::Note,
            model: String::new(),
            text: body.into(),
            confidence: 1.0,
        };
    }

    // Split on first whitespace — matches Python's split(maxsplit=1)
    let (first, rest) = match text.split_once(char::is_whitespace) {
        Some((f, r)) => (f, r.trim()),
        None => (text, ""),
    };

    // Normalize first word: lowercase, strip trailing punctuation
    let first_clean = first.to_lowercase();
    let first_clean = first_clean.trim_end_matches(&['.', ',', '!', '?', ':', ';'][..]);

    // A spoken "edit" usually has no dictated colon. Keep other verbs
    // colon-only so ordinary dictation such as "change the setting" stays Type.
    if first_clean == "edit" {
        return RouteResult {
            route: RouteType::Edit,
            model: "local".into(),
            text: rest.into(),
            confidence: 1.0,
        };
    }

    if first_clean == "timer" {
        return RouteResult {
            route: RouteType::Timer,
            model: String::new(),
            text: rest.into(),
            confidence: 1.0,
        };
    }

    if LOCAL_TRIGGERS.contains(&first_clean) {
        return RouteResult {
            route: RouteType::Local,
            model: "local".into(),
            text: rest.into(),
            confidence: 1.0,
        };
    }

    // Default: type the original text as-is
    RouteResult {
        route: RouteType::Type,
        model: String::new(),
        text: text.into(),
        confidence: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_input() {
        let result = route("");
        assert_eq!(result.route, RouteType::Type);
        assert!(result.text.is_empty());
    }

    #[test]
    fn test_whitespace_only() {
        let result = route("   ");
        assert_eq!(result.route, RouteType::Type);
        assert!(result.text.is_empty());
    }

    #[test]
    fn test_edit_triggers() {
        for trigger in &["edit:", "fix:", "change:", "rewrite:", "transform:"] {
            let input = format!("{} make this better", trigger);
            let result = route(&input);
            assert_eq!(
                result.route,
                RouteType::Edit,
                "Failed for trigger: {}",
                trigger
            );
            assert_eq!(result.text, "make this better");
            assert_eq!(result.model, "local");
        }
    }

    #[test]
    fn test_edit_trigger_case_insensitive() {
        let result = route("Edit: fix this sentence");
        assert_eq!(result.route, RouteType::Edit);
        assert_eq!(result.text, "fix this sentence");
    }

    #[test]
    fn spoken_edit_prefix_and_ordinary_verbs() {
        assert_eq!(route("Edit make this formal").route, RouteType::Edit);
        assert_eq!(route("edit").route, RouteType::Edit);
        for text in [
            "editable text",
            "change the setting",
            "rewrite history",
            "fix the deployment",
        ] {
            assert_eq!(route(text).route, RouteType::Type);
        }
    }

    #[test]
    fn scratchpad_triggers_route_to_note_with_the_body_alone() {
        for (said, body) in [
            ("Note: buy milk.", "buy milk."),
            ("note, buy milk", "buy milk"),
            ("NOTE:buy milk", "buy milk"),
            ("Note to self: call the dentist.", "call the dentist."),
            ("note to self call the dentist", "call the dentist"),
            ("Quick note, the meeting moved", "the meeting moved"),
            ("quick note the meeting moved", "the meeting moved"),
            ("Take a note: ship it", "ship it"),
            ("make a note ship it", "ship it"),
            ("New note - ship it", "ship it"),
            ("add a note. ship it", "ship it"),
        ] {
            let r = route(said);
            assert_eq!(r.route, RouteType::Note, "{said}");
            assert_eq!(r.text, body, "{said}");
        }
    }

    #[test]
    fn a_bare_note_word_and_lookalikes_stay_typed() {
        // The false positive that matters: prose must still reach the window.
        for text in [
            "note that the deadline moved",
            "Note the following changes",
            "notes to self are useful",
            "quick notebook",
            "notebook paper",
            "new notes arrived",
            "take a notebook",
            "I made a note of it",
            "keynote: opening",
            "make a note",
        ] {
            let expected = if text == "make a note" {
                // The phrase alone is still a trigger; it just has no body.
                RouteType::Note
            } else {
                RouteType::Type
            };
            assert_eq!(route(text).route, expected, "{text}");
        }
        assert_eq!(route("make a note").text, "");
        assert_eq!(route("note:").text, "");
    }

    #[test]
    fn other_triggers_keep_priority_over_the_scratchpad() {
        assert_eq!(route("edit: note: x").route, RouteType::Edit);
        assert_eq!(route("timer note: x").route, RouteType::Timer);
        assert_eq!(route("easy note: x").route, RouteType::Local);
    }

    #[test]
    fn test_timer_trigger() {
        let result = route("timer 5 minutes");
        assert_eq!(result.route, RouteType::Timer);
        assert_eq!(result.text, "5 minutes");
    }

    #[test]
    fn test_timer_trigger_alone() {
        let result = route("timer");
        assert_eq!(result.route, RouteType::Timer);
        assert!(result.text.is_empty());
    }

    #[test]
    fn test_timer_with_punctuation() {
        let result = route("Timer. 10 minutes");
        assert_eq!(result.route, RouteType::Timer);
        assert_eq!(result.text, "10 minutes");
    }

    #[test]
    fn test_local_triggers() {
        for trigger in &["simple", "easy", "medium", "hard"] {
            let input = format!("{} what is the weather", trigger);
            let result = route(&input);
            assert_eq!(
                result.route,
                RouteType::Local,
                "Failed for trigger: {}",
                trigger
            );
            assert_eq!(result.text, "what is the weather");
            assert_eq!(result.model, "local");
        }
    }

    #[test]
    fn test_local_trigger_case_insensitive() {
        let result = route("Easy what is Rust");
        assert_eq!(result.route, RouteType::Local);
        assert_eq!(result.text, "what is Rust");
    }

    #[test]
    fn test_local_trigger_with_punctuation() {
        let result = route("hard, explain quantum physics");
        assert_eq!(result.route, RouteType::Local);
        assert_eq!(result.text, "explain quantum physics");
    }

    #[test]
    fn test_default_type_route() {
        let result = route("hello world");
        assert_eq!(result.route, RouteType::Type);
        assert_eq!(result.text, "hello world");
    }

    #[test]
    fn test_default_preserves_case() {
        let result = route("Hello World");
        assert_eq!(result.route, RouteType::Type);
        assert_eq!(result.text, "Hello World");
    }

    #[test]
    fn test_single_word_non_trigger() {
        let result = route("hello");
        assert_eq!(result.route, RouteType::Type);
        assert_eq!(result.text, "hello");
    }

    #[test]
    fn test_confidence_always_one() {
        let cases = vec!["", "hello", "timer 5m", "easy test", "edit: fix"];
        for input in cases {
            let result = route(input);
            assert!((result.confidence - 1.0).abs() < f64::EPSILON);
        }
    }
}
