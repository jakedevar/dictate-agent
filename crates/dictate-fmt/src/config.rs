use serde::Deserialize;

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
