use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct WhisperConfig {
    /// Path to GGUF model file, e.g. ~/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin
    pub model_path: String,
    /// "cuda" or "cpu"
    pub device: String,
    /// Threshold for filtering non-speech (0.0-1.0, higher = stricter)
    pub no_speech_threshold: f32,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            model_path: "~/.local/share/dictate-agent/models/ggml-large-v3-turbo.bin".into(),
            device: "cuda".into(),
            no_speech_threshold: 0.6,
        }
    }
}

// Deserialize/Default behavior for this type is exercised by the aggregate
// `Config` tests in dictate-core::config (test_default_values,
// test_toml_parsing_full, etc.) — not duplicated here.

/// Expand ~ to home directory in a path string.
/// Duplicated (verbatim, ~8 lines) from dictate-core::config to avoid a
/// circular crate dependency: dictate-core depends on dictate-stt for
/// `Transcriber`, so dictate-stt cannot depend back on dictate-core for this
/// trivial, stateless helper. See S00 handoff for rationale.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs_path_home().join(rest)
    } else if path == "~" {
        dirs_path_home()
    } else {
        PathBuf::from(path)
    }
}

fn dirs_path_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/user"))
}
