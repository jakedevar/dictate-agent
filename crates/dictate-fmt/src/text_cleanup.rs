//! Model-artifact scrub for free-form LLM answers (the LOCAL route).
//!
//! Only strings that are artifacts of the *model* are removed. A trailing
//! "Thank you." is deliberately not one of them: it is ordinary English that
//! a user may dictate or a model may legitimately write (#1069, #1072).
//! Whisper's habit of hallucinating "Thank you." on trailing silence is a
//! property of the *transcript*, and the S20 scrub rule
//! (`text::rules::scrub`) handles it there, only when it is its own sentence.

/// Trailing strings a model can leak into its answer.
const TRAILING_ARTIFACTS: &[&str] = &["/no_think"];

/// Remove known model artifacts from the end of user-visible model output.
pub fn scrub_returned_text(text: &str) -> String {
    let mut cleaned = text.trim().to_string();

    loop {
        let trimmed = cleaned.trim_end().to_string();
        let mut next = None;

        for artifact in TRAILING_ARTIFACTS {
            if trimmed.len() < artifact.len() {
                continue;
            }

            let start = trimmed.len() - artifact.len();
            if let Some(candidate) = trimmed.get(start..) {
                if candidate.eq_ignore_ascii_case(artifact) {
                    next = Some(trimmed[..start].trim_end().to_string());
                    break;
                }
            }
        }

        match next {
            Some(updated) => cleaned = updated,
            None => return trimmed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::scrub_returned_text;

    #[test]
    fn strips_trailing_no_think_case_insensitively() {
        assert_eq!(scrub_returned_text("Hello world /NO_THINK"), "Hello world");
    }

    #[test]
    fn strips_repeated_trailing_artifacts() {
        assert_eq!(
            scrub_returned_text("Hello world /no_think /NO_THINK"),
            "Hello world"
        );
    }

    #[test]
    fn keeps_non_trailing_artifacts() {
        assert_eq!(
            scrub_returned_text("/no_think is a flag"),
            "/no_think is a flag"
        );
    }

    /// #1069 / #1072: a dictated or written "thank you." is text, not an
    /// artifact, and survives the scrub.
    #[test]
    fn never_strips_a_thank_you() {
        for text in [
            "I just wanted to thank you.",
            "Thanks for the report. Thank you.",
            "Thank you.",
            "Hello world THANK YOU.",
        ] {
            assert_eq!(scrub_returned_text(text), text);
        }
        assert_eq!(
            scrub_returned_text("Thanks for the report. Thank you. /no_think"),
            "Thanks for the report. Thank you."
        );
    }
}
