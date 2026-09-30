use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DictionaryConfig {
    pub enabled: bool,
    pub db_path: String,
    pub stt_bias: bool,
    pub max_prompt_chars: usize,
    pub recase_phrases: bool,
    pub fuzzy: bool,
    pub fuzzy_threshold: f64,
}

impl Default for DictionaryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            db_path: String::new(),
            stt_bias: true,
            max_prompt_chars: 400,
            recase_phrases: true,
            fuzzy: false,
            fuzzy_threshold: 0.9,
        }
    }
}

impl DictionaryConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.max_prompt_chars > 0,
            "dictionary.max_prompt_chars must be positive"
        );
        anyhow::ensure!(
            self.fuzzy_threshold.is_finite() && (0.8..=1.0).contains(&self.fuzzy_threshold),
            "dictionary.fuzzy_threshold must be in 0.8..=1.0"
        );
        Ok(())
    }
    pub fn path(&self) -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        if let Some(rest) = self.db_path.strip_prefix("~/") {
            return home.join(rest);
        }
        if !self.db_path.is_empty() {
            return PathBuf::from(&self.db_path);
        }
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("dictated/dictionary.db")
    }
}
