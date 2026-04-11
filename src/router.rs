#[derive(Debug, Clone, PartialEq)]
pub enum RouteType {
    Type,
    Local,
    Timer,
    Edit,
    Command,
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

/// Route transcribed text by keyword prefix.
/// Direct port of dictate/router.py:43-79.
///
/// Priority:
/// 1. Empty text → Type
/// 2. Edit triggers (colon-suffixed) → Edit
/// 3. "timer" prefix → Timer
/// 4. LOCAL_TRIGGERS prefix → Local
/// 5. Default → Type (with original text preserved)
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

    // Split on first whitespace — matches Python's split(maxsplit=1)
    let (first, rest) = match text.split_once(char::is_whitespace) {
        Some((f, r)) => (f, r.trim()),
        None => (text, ""),
    };

    // Normalize first word: lowercase, strip trailing punctuation
    let first_clean = first.to_lowercase();
    let first_clean = first_clean.trim_end_matches(&['.', ',', '!', '?', ':', ';'][..]);

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
            assert_eq!(result.route, RouteType::Edit, "Failed for trigger: {}", trigger);
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
            assert_eq!(result.route, RouteType::Local, "Failed for trigger: {}", trigger);
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
