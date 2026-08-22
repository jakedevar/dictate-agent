use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct HistoryConfig {
    pub enabled: bool,
    /// Empty string means default: ~/.local/share/dictated/history.db
    pub db_path: String,
    pub max_response_length: usize,
    /// Global no-store mode. Session privacy can enable the same guarantee
    /// for one dictation without changing this setting.
    pub privacy_mode: bool,
    /// Delete rows older than this many UTC days. `None` retains history.
    pub retention_days: Option<u32>,
    /// Import the Python reference daemon's database once, if it exists.
    pub import_python_db: bool,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            db_path: String::new(),
            max_response_length: 10000,
            privacy_mode: false,
            retention_days: None,
            import_python_db: false,
        }
    }
}

// Deserialize/Default behavior for this type is exercised by the aggregate
// `Config` tests in dictate-core::config (test_default_values,
// test_toml_parsing_full, etc.) — not duplicated here.

/// Expand ~ to home directory in a path string.
/// Duplicated (verbatim, ~8 lines) from dictate-core::config to avoid a
/// circular crate dependency: dictate-core depends on dictate-history for
/// `HistoryStore`, so dictate-history cannot depend back on dictate-core for
/// this trivial, stateless helper. See S00 handoff for rationale.
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
