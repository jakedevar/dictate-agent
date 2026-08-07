use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct OutputConfig {
    pub auto_type: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self { auto_type: true }
    }
}

// Deserialize/Default behavior for this type is exercised by the aggregate
// `Config` tests in dictate-core::config (test_default_values,
// test_toml_parsing_full, etc.) — not duplicated here.
