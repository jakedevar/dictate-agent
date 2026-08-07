use serde::Deserialize;
use std::path::{Path, PathBuf};

pub use dictate_fmt::GrammarConfig;
pub use dictate_history::HistoryConfig;
pub use dictate_inject::OutputConfig;
pub use dictate_stt::WhisperConfig;

// XDG paths
const CONFIG_DIR: &str = "dictate-agent";
const CONFIG_FILE: &str = "config.toml";
const PID_FILE: &str = "dictate.pid";
const MEDIA_STATE_FILE: &str = "media_was_playing";

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub whisper: WhisperConfig,
    pub grammar: GrammarConfig,
    pub local: LocalConfig,
    pub output: OutputConfig,
    pub notifications: NotificationConfig,
    pub history: HistoryConfig,
    pub timer: TimerConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct LocalConfig {
    pub host: String,
    pub model: String,
    pub timeout_s: f64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub timeout_ms: u32,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct TimerConfig {
    pub sound_enabled: bool,
    pub sound_file: String,
}

// --- Default implementations ---

// Config derives Default since all fields implement Default
// (the #[derive(Default)] would call each field's Default impl; WhisperConfig,
// GrammarConfig, OutputConfig, HistoryConfig now derive Default in their
// owning crates — dictate-stt, dictate-fmt, dictate-inject, dictate-history)

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            host: "http://localhost:11434".into(),
            model: "qwen3:14b".into(),
            timeout_s: 120.0,
        }
    }
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: 1250,
        }
    }
}

impl Default for TimerConfig {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            sound_file: "~/.config/dictate-agent/sounds/timer_alarm.wav".into(),
        }
    }
}

// --- Path helpers ---

/// Returns the config directory: XDG_CONFIG_HOME/dictate-agent or ~/.config/dictate-agent
pub fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_path_home().join(".config"));
    base.join(CONFIG_DIR)
}

/// Returns the PID file path
pub fn pid_file_path() -> PathBuf {
    config_dir().join(PID_FILE)
}

/// Returns the media state file path
pub fn media_state_path() -> PathBuf {
    config_dir().join(MEDIA_STATE_FILE)
}

/// Expand ~ to home directory in a path string
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs_path_home().join(rest)
    } else if path == "~" {
        dirs_path_home()
    } else {
        PathBuf::from(path)
    }
}

/// Get home directory
fn dirs_path_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/user"))
}

/// Load config from TOML file. Falls back to defaults if file doesn't exist.
/// serde(default) on every struct means missing sections/fields use defaults.
/// Unknown keys in the TOML are silently ignored.
pub fn load_config(path: Option<&Path>) -> anyhow::Result<Config> {
    let config_path = match path {
        Some(p) => p.to_path_buf(),
        None => config_dir().join(CONFIG_FILE),
    };

    if !config_path.exists() {
        tracing::info!(
            "No config file at {}, using defaults",
            config_path.display()
        );
        return Ok(Config::default());
    }

    let contents = std::fs::read_to_string(&config_path)?;
    let mut config: Config = toml::from_str(&contents)?;

    // Expand tildes in path fields
    config.whisper.model_path = expand_tilde(&config.whisper.model_path)
        .to_string_lossy()
        .into_owned();
    if !config.history.db_path.is_empty() {
        config.history.db_path = expand_tilde(&config.history.db_path)
            .to_string_lossy()
            .into_owned();
    }
    config.timer.sound_file = expand_tilde(&config.timer.sound_file)
        .to_string_lossy()
        .into_owned();

    tracing::info!("Config loaded from {}", config_path.display());
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_values() {
        let config = Config::default();
        assert_eq!(config.whisper.device, "cuda");
        assert!((config.whisper.no_speech_threshold - 0.6).abs() < f32::EPSILON);
        assert!(config.grammar.enabled);
        assert_eq!(config.grammar.host, "http://localhost:11434");
        assert_eq!(config.grammar.model, "qwen3:0.6b");
        assert!((config.grammar.timeout_s - 10.0).abs() < f64::EPSILON);
        assert_eq!(config.grammar.min_words, 3);
        assert_eq!(config.local.model, "qwen3:14b");
        assert!(config.output.auto_type);
        assert!(config.notifications.enabled);
        assert_eq!(config.notifications.timeout_ms, 1250);
        assert!(config.history.enabled);
        assert!(config.history.db_path.is_empty());
        assert_eq!(config.history.max_response_length, 10000);
        assert!(config.timer.sound_enabled);
    }

    #[test]
    fn test_toml_parsing_full() {
        let toml_str = r#"
[whisper]
model_path = "/tmp/test-model.bin"
device = "cpu"
no_speech_threshold = 0.8

[grammar]
enabled = false
host = "http://example.com:11434"
model = "test-model"
timeout_s = 5.0
min_words = 5

[local]
host = "http://example.com:11434"
model = "test-local"
timeout_s = 60.0

[output]
auto_type = false

[notifications]
enabled = false
timeout_ms = 5000

[history]
enabled = false
db_path = "/tmp/test.db"
max_response_length = 500

[timer]
sound_enabled = false
sound_file = "/tmp/alarm.wav"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.whisper.model_path, "/tmp/test-model.bin");
        assert_eq!(config.whisper.device, "cpu");
        assert!((config.whisper.no_speech_threshold - 0.8).abs() < f32::EPSILON);
        assert!(!config.grammar.enabled);
        assert_eq!(config.grammar.model, "test-model");
        assert_eq!(config.grammar.min_words, 5);
        assert!(!config.output.auto_type);
        assert!(!config.notifications.enabled);
        assert_eq!(config.notifications.timeout_ms, 5000);
        assert!(!config.history.enabled);
        assert_eq!(config.history.db_path, "/tmp/test.db");
        assert!(!config.timer.sound_enabled);
    }

    #[test]
    fn test_toml_missing_sections() {
        // Only whisper section provided — everything else should use defaults
        let toml_str = r#"
[whisper]
device = "cpu"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.whisper.device, "cpu");
        // model_path should be the default since it wasn't specified
        assert!(config
            .whisper
            .model_path
            .contains("ggml-large-v3-turbo.bin"));
        // All other sections should be defaults
        assert!(config.grammar.enabled);
        assert_eq!(config.grammar.model, "qwen3:0.6b");
        assert!(config.output.auto_type);
        assert!(config.timer.sound_enabled);
    }

    #[test]
    fn test_toml_unknown_keys_ignored() {
        let toml_str = r#"
[whisper]
device = "cpu"
unknown_key = "should be ignored"

[some_unknown_section]
foo = "bar"
"#;
        // This should not error — serde deny_unknown_fields is NOT set
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_ok());
    }

    #[test]
    fn test_tilde_expansion() {
        let expanded = expand_tilde("~/test/path");
        assert!(!expanded.to_string_lossy().starts_with('~'));
        assert!(expanded.to_string_lossy().ends_with("test/path"));

        // Non-tilde paths pass through unchanged
        let unchanged = expand_tilde("/absolute/path");
        assert_eq!(unchanged, PathBuf::from("/absolute/path"));

        let relative = expand_tilde("relative/path");
        assert_eq!(relative, PathBuf::from("relative/path"));
    }

    #[test]
    fn test_empty_toml() {
        let config: Config = toml::from_str("").unwrap();
        // All defaults
        assert_eq!(config.whisper.device, "cuda");
        assert!(config.grammar.enabled);
    }

    #[test]
    fn test_load_config_missing_file() {
        let result = load_config(Some(Path::new("/tmp/nonexistent-dictate-config.toml")));
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.whisper.device, "cuda"); // defaults
    }
}
