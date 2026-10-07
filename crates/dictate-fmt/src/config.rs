use serde::Deserialize;

/// The deprecated `[grammar]` section, kept only as a parse target: S21's
/// [`crate::llm::LlmConfig`] reads these keys as an alias.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct GrammarConfig {
    pub enabled: bool,
    pub host: String,
    pub model: String,
    pub timeout_s: f64,
    pub min_words: usize,
}

impl Default for GrammarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: "http://localhost:11434".into(),
            model: "qwen3:0.6b".into(),
            timeout_s: 10.0,
            min_words: 3,
        }
    }
}

// Deserialize/Default behavior for this type is exercised by the aggregate
// `Config` tests in dictate-core::config (test_default_values,
// test_toml_parsing_full, etc.) — not duplicated here.

/// `[format]`: the deterministic text chain that runs between STT and routing.
///
/// The LLM pass lives under `[format.llm]` (S21); the old `[grammar]`
/// section is only an alias for it.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct FormatConfig {
    /// Run the deterministic rules at all. When `false` the session reports
    /// `fmt_rules` as `skipped{disabled}` and the raw transcript goes straight
    /// to routing. Protected-span detection still guards the LLM pass, because
    /// that guard is a safety property, not a formatting preference.
    pub enabled: bool,
    /// One toggle per rule.
    pub rules: RulesConfig,
    /// `[format.llm]` (S21). The daemon's loader re-derives this through
    /// [`LlmConfig::from_document`](crate::llm::LlmConfig::from_document) so
    /// the deprecated `[grammar]` keys still apply as an alias.
    pub llm: crate::llm::LlmConfig,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rules: RulesConfig::default(),
            llm: crate::llm::LlmConfig::default(),
        }
    }
}

/// `[format.rules]`: one switch per named stage.
///
/// Every default is the conservative choice: a rule that is on by default
/// never changes what the speaker meant. The two spoken-command rules are off
/// because Jake dictates code prompts in which "new line" and "period" are
/// often literal words; S23 profiles enable them per app.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct RulesConfig {
    /// The historical Whisper mis-hearing fixes (`cloud` → `Claude`,
    /// `create plan` → `/create_plan`, …), word-boundary aware.
    pub builtin_corrections: bool,
    /// Trailing Whisper/LLM artifacts (a lone `Thank you.`, `/no_think`) and
    /// whitespace normalization.
    pub hallucination_scrub: bool,
    /// Standalone disfluencies: um, uh, erm, hmm, …
    pub fillers: bool,
    /// Immediate function-word repeats ("the the") and cut-off fragments.
    pub stutters: bool,
    /// Spoken numbers to digits where that is unambiguous.
    pub numbers: bool,
    /// Sentence-start capitals and the pronoun "I".
    pub casing: bool,
    /// Space collapsing and punctuation spacing.
    pub spacing: bool,
    /// A final "." on utterances of four or more words.
    pub terminal_punctuation: bool,
    /// "comma", "period", "question mark", … → punctuation.
    pub spoken_punctuation: bool,
    /// "new line" / "new paragraph" → line breaks.
    pub spoken_line_breaks: bool,
}

impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            builtin_corrections: true,
            hallucination_scrub: true,
            fillers: true,
            stutters: true,
            numbers: true,
            casing: true,
            spacing: true,
            terminal_punctuation: true,
            spoken_punctuation: false,
            spoken_line_breaks: false,
        }
    }
}
