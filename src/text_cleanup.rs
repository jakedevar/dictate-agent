const TRAILING_ARTIFACTS: &[&str] = &["thank you.", "/no_think"];

/// Remove known spurious trailer strings from user-visible model output.
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
    fn strips_trailing_thank_you_case_insensitively() {
        assert_eq!(scrub_returned_text("Hello world THANK YOU."), "Hello world");
    }

    #[test]
    fn strips_trailing_no_think_case_insensitively() {
        assert_eq!(scrub_returned_text("Hello world /NO_THINK"), "Hello world");
    }

    #[test]
    fn strips_repeated_trailing_artifacts() {
        assert_eq!(
            scrub_returned_text("Hello world Thank You. /NO_THINK thank you."),
            "Hello world"
        );
    }

    #[test]
    fn keeps_non_trailing_artifacts() {
        assert_eq!(
            scrub_returned_text("Thank you. for listening"),
            "Thank you. for listening"
        );
    }
}
