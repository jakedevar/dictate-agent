//! Application context: where a session's text is going.
//!
//! The context engine (S23) resolves this from the focused window when a
//! session starts; a remote client (S33) can instead name its target through
//! [`SessionOptions::app`](crate::SessionOptions::app). Formatting (S20/S21)
//! reads it for tone and structure, the dictionary and snippets (S22/S24) for
//! per-app scope, and injection (S13) for the per-app paste/type policy.
//!
//! These types live in the protocol crate, rather than in `dictate-context`,
//! because they cross the wire (events, history, the UI's profile editor) and
//! because every consumer above must share one definition without depending
//! on the context engine itself.
//!
//! # Absence is normal
//!
//! A headless host, the network API, and GNOME on Wayland without a Shell
//! extension have no focused-window context at all. Consumers take
//! `Option<AppContext>` and must give `None` a defined default path — it is
//! the ordinary baseline on those platforms, never an error (R3 constraint).

use serde::{Deserialize, Serialize};

open_str_enum! {
    /// Coarse kind of application receiving the text.
    ///
    /// A category selects defaults — tone, whether the LLM pass runs, and the
    /// injection method. A profile can override any of them for one app.
    pub enum AppCategory {
        /// Terminal emulators, including coding agents running inside them.
        Terminal => "terminal",
        /// Code editors and IDEs.
        Editor => "editor",
        /// Web browsers, when nothing more specific is known.
        Browser => "browser",
        /// Chat and messaging apps.
        Chat => "chat",
        /// Email clients and webmail.
        Email => "email",
        /// Documents and notes.
        Document => "document",
        /// Anything else, and the default when an app is unrecognized.
        Other => "other",
    }
    default = Other;
}

open_str_enum! {
    /// Writing register the formatting pass aims for.
    ///
    /// Mirrors Wispr Flow's per-app styles. `Neutral` — clean the text up
    /// without shifting its register — is the default, because changing tone
    /// changes the words a person said and must therefore be opted into per
    /// category or profile.
    pub enum Tone {
        /// Complete sentences, no slang or contractions added.
        Formal => "formal",
        /// Clean up only; keep the speaker's own register.
        Neutral => "neutral",
        /// Relaxed punctuation and capitalization, contractions kept.
        Casual => "casual",
        /// Chat-style: minimal punctuation, lowercase allowed.
        VeryCasual => "very_casual",
    }
    default = Neutral;
}

/// The application a session's text is destined for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppContext {
    /// Stable application identifier used for profile matching and per-app
    /// scope: on X11 the lowercased `WM_CLASS` class; over the network API,
    /// whatever the client supplied in `SessionOptions::app`.
    pub app: String,

    /// Window title when the session started.
    ///
    /// Privacy-sensitive — a title can carry a document name or a message
    /// preview — so it must not be persisted when privacy mode applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// The category the app resolved to.
    #[serde(default)]
    pub category: AppCategory,

    /// Name of the configured profile that matched, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

impl AppContext {
    /// Context for `app` with no title, the default category, and no profile.
    pub fn new(app: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            title: None,
            category: AppCategory::default(),
            profile: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_the_conservative_choices() {
        assert_eq!(AppCategory::default(), AppCategory::Other);
        assert_eq!(Tone::default(), Tone::Neutral);
    }

    #[test]
    fn a_bare_app_name_is_a_complete_context() {
        let ctx: AppContext = serde_json::from_value(json!({"app": "slack"})).unwrap();
        assert_eq!(ctx, AppContext::new("slack"));
    }

    #[test]
    fn unknown_categories_and_tones_survive_a_round_trip() {
        let ctx: AppContext =
            serde_json::from_value(json!({"app": "x", "category": "spreadsheet"})).unwrap();
        assert!(!ctx.category.is_known());
        assert_eq!(
            serde_json::to_value(&ctx).unwrap()["category"],
            json!("spreadsheet")
        );

        let tone: Tone = serde_json::from_value(json!("excited")).unwrap();
        assert_eq!(tone, Tone::Unknown("excited".into()));
        assert_eq!(serde_json::to_value(&tone).unwrap(), json!("excited"));
    }
}
