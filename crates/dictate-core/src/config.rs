use serde::Deserialize;
use std::path::{Path, PathBuf};

pub use dictate_audio::AudioConfig;
pub use dictate_fmt::GrammarConfig;
pub use dictate_history::HistoryConfig;
pub use dictate_inject::OutputConfig;
pub use dictate_stt::WhisperConfig;
pub use dictate_vad::VadConfig;

// XDG paths
const CONFIG_DIR: &str = "dictate-agent";
const CONFIG_FILE: &str = "config.toml";
const PID_FILE: &str = "dictate.pid";
const MEDIA_STATE_FILE: &str = "media_was_playing";

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub audio: AudioConfig,
    pub whisper: WhisperConfig,
    pub vad: VadConfig,
    pub grammar: GrammarConfig,
    pub local: LocalConfig,
    pub output: OutputConfig,
    pub notifications: NotificationConfig,
    pub history: HistoryConfig,
    pub timer: TimerConfig,
    pub hotkey: HotkeyConfig,
    pub upload: UploadConfig,
}

/// Limits on audio a client uploads with `transcribe_audio`.
///
/// Bounded on purpose: an upload is decoded and resampled in memory, and the
/// local socket must not be a way to make the daemon allocate without limit.
/// The defaults comfortably cover Jake's longest dictation (about ten minutes
/// of speech) and a CD-quality stereo WAV of it is *not* expected to fit —
/// raise `max_bytes` for that.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct UploadConfig {
    /// Longest accepted clip, in seconds of audio.
    pub max_audio_seconds: u32,
    /// Largest accepted payload, in bytes of (decoded) audio data.
    pub max_bytes: u64,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            max_audio_seconds: 600,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

impl UploadConfig {
    /// The protocol [`Limits`](dictate_proto::Limits) these settings imply.
    ///
    /// `max_message_bytes` is the *encoded* line: base64 inflates by 4/3, and
    /// the JSON envelope adds a little more.
    #[must_use]
    pub fn limits(&self) -> dictate_proto::Limits {
        let encoded = self
            .max_bytes
            .saturating_mul(4)
            .div_ceil(3)
            .saturating_add(4096);
        dictate_proto::Limits {
            max_message_bytes: u32::try_from(encoded).unwrap_or(u32::MAX),
            max_audio_ms: Some(self.max_audio_seconds.saturating_mul(1000)),
            ..dictate_proto::Limits::default()
        }
    }
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

/// Configuration for the optional evdev global-hotkey service.
///
/// Key values are Linux input-event key codes (the values named `KEY_*` in
/// `/usr/include/linux/input-event-codes.h`). Empty chords are disabled.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct HotkeyConfig {
    pub enabled: bool,
    pub devices: Vec<String>,
    pub hold_to_talk: Vec<u16>,
    pub toggle: Vec<u16>,
    pub cancel: Vec<u16>,
    pub release_grace_ms: u64,
    pub double_tap_ms: u64,
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

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            // Signal/WM keybindings remain first-class. Opt in to evdev only
            // after selecting a device that this user may read.
            enabled: false,
            devices: Vec::new(),
            hold_to_talk: Vec::new(),
            toggle: Vec::new(),
            cancel: Vec::new(),
            release_grace_ms: 45,
            double_tap_ms: 320,
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

/// What loading a configuration file found, beyond the values themselves.
///
/// A config file is the one place a user's intent enters the daemon, and every
/// key the daemon does not act on is a silent divergence between what the user
/// believes and what runs. So loading is never silent: unknown sections and
/// keys, legacy spellings that were mapped, and values that were clamped all
/// end up here, and are logged once at startup and shown by
/// `dictated --check-config` and `dictate doctor`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigReport {
    /// The file that was (or would have been) read.
    pub path: PathBuf,
    /// Whether that file exists. A missing file is not an error: defaults apply.
    pub existed: bool,
    /// Things that were accepted but deserve attention: ignored keys, mapped
    /// legacy values.
    pub warnings: Vec<String>,
    /// Things that make the configuration unusable.
    pub errors: Vec<String>,
}

/// Python-era faster-whisper keys under `[whisper]` that have no whisper.cpp
/// equivalent.
const LEGACY_WHISPER_KEYS: &[&str] = &[
    "assistant_model",
    "compute_type",
    "use_speculative_decoding",
    "chunk_length_s",
    "batch_size",
];

/// Hugging Face model ids the Python daemon used, and the catalog entry each
/// corresponds to.
const HF_MODEL_ALIASES: &[(&str, &str)] = &[
    ("openai/whisper-large-v3-turbo", "large-v3-turbo"),
    ("openai/whisper-tiny.en", "tiny.en"),
];

/// Load config from a TOML file. Falls back to defaults if the file doesn't
/// exist.
///
/// Unknown sections and keys are **not** silently ignored: each is logged once
/// as a warning (and returned by [`load_config_with_report`]). Legacy
/// Python-era spellings are mapped rather than dropped. A structurally invalid
/// file, or a value that would crash the daemon later, is an error here rather
/// than a surprise at the first dictation.
pub fn load_config(path: Option<&Path>) -> anyhow::Result<Config> {
    let (config, report) = load_config_with_report(path)?;
    for warning in &report.warnings {
        tracing::warn!("config: {warning}");
    }
    if !report.errors.is_empty() {
        anyhow::bail!(
            "invalid configuration in {}:\n  - {}",
            report.path.display(),
            report.errors.join("\n  - ")
        );
    }
    Ok(config)
}

/// As [`load_config`], but returns what was found instead of logging it, and
/// returns validation problems in the report rather than failing on them.
///
/// # Errors
///
/// Only when the file cannot be read or parsed at all.
pub fn load_config_with_report(path: Option<&Path>) -> anyhow::Result<(Config, ConfigReport)> {
    let config_path = match path {
        Some(p) => p.to_path_buf(),
        None => config_dir().join(CONFIG_FILE),
    };

    if !config_path.exists() {
        tracing::info!(
            "No config file at {}, using defaults",
            config_path.display()
        );
        let config = finalize(Config::default());
        let errors = validate(&config);
        return Ok((
            config,
            ConfigReport {
                path: config_path,
                existed: false,
                warnings: Vec::new(),
                errors,
            },
        ));
    }

    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", config_path.display()))?;
    let (config, mut report) = parse_config(&contents)
        .map_err(|e| anyhow::anyhow!("parsing {}: {e}", config_path.display()))?;
    report.path = config_path.clone();
    report.existed = true;
    tracing::info!("Config loaded from {}", config_path.display());
    Ok((config, report))
}

/// Parse configuration text, applying legacy mappings and collecting warnings.
///
/// Separated from the file handling so the compatibility rules are testable
/// against fixture strings.
///
/// # Errors
///
/// If the text is not valid TOML, or a known key has the wrong type.
pub fn parse_config(contents: &str) -> anyhow::Result<(Config, ConfigReport)> {
    let mut value: toml::Value = toml::from_str(contents)?;
    let mut warnings = Vec::new();
    if let Some(table) = value.as_table_mut() {
        map_legacy_router(table, &mut warnings);
        map_legacy_whisper_model(table, &mut warnings);
    }

    let mut ignored: Vec<String> = Vec::new();
    let config: Config = serde_ignored::deserialize(value.clone(), |path| {
        ignored.push(path.to_string());
    })?;

    for path in ignored {
        warnings.push(describe_ignored(&path, &value));
    }

    let config = finalize(config);
    let mut errors = validate(&config);
    warnings.extend(soft_checks(&config));
    errors.dedup();
    Ok((
        config,
        ConfigReport {
            path: PathBuf::new(),
            existed: true,
            warnings,
            errors,
        },
    ))
}

/// `[router]` was the Python daemon's name for what dictated calls `[local]`
/// (the Ollama server and model behind the `local` route). The June binary
/// ignored it; mapping it keeps a Python-era file meaning what it says.
fn map_legacy_router(table: &mut toml::map::Map<String, toml::Value>, warnings: &mut Vec<String>) {
    let Some(toml::Value::Table(mut router)) = table.remove("router") else {
        return;
    };
    let mut mapped = Vec::new();
    for (legacy, current) in [
        ("ollama_host", "host"),
        ("ollama_model", "model"),
        ("ollama_timeout_s", "timeout_s"),
    ] {
        let Some(v) = router.remove(legacy) else {
            continue;
        };
        let local = table
            .entry("local")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        if let toml::Value::Table(local) = local {
            if local.contains_key(current) {
                warnings.push(format!(
                    "[router] {legacy} ignored: [local] {current} is set and wins"
                ));
            } else {
                local.insert(current.to_string(), v);
                mapped.push(format!("router.{legacy} -> local.{current}"));
            }
        }
    }
    if !mapped.is_empty() {
        warnings.push(format!(
            "[router] is the Python-era name for [local]; mapped {} \
             (rename the section to silence this)",
            mapped.join(", ")
        ));
    }
    // Anything left in `[router]` is genuinely unknown and is reported as such.
    if !router.is_empty() {
        table.insert("router".into(), toml::Value::Table(router));
    }
}

/// The Python daemon named Whisper models by Hugging Face id; whisper.cpp uses
/// catalog names. Map the ones that correspond, and refuse to guess for the
/// rest.
fn map_legacy_whisper_model(
    table: &mut toml::map::Map<String, toml::Value>,
    warnings: &mut Vec<String>,
) {
    let Some(toml::Value::Table(whisper)) = table.get_mut("whisper") else {
        return;
    };
    let Some(toml::Value::String(model)) = whisper.get("model") else {
        return;
    };
    if !model.contains('/') {
        return;
    }
    let model = model.clone();
    let default = WhisperConfig::default().model;
    let replacement =
        if let Some((_, catalog)) = HF_MODEL_ALIASES.iter().find(|(hf, _)| *hf == model) {
            warnings.push(format!(
                "whisper.model '{model}' is a Hugging Face id; using the whisper.cpp \
             catalog model '{catalog}' (set whisper.model = \"{catalog}\" to silence this)"
            ));
            (*catalog).to_string()
        } else {
            warnings.push(format!(
                "whisper.model '{model}' is a Hugging Face id with no whisper.cpp catalog \
             equivalent; using '{default}'"
            ));
            default
        };
    whisper.insert("model".into(), toml::Value::String(replacement));
}

/// Explain one key the deserializer ignored.
fn describe_ignored(path: &str, root: &toml::Value) -> String {
    let segments: Vec<&str> = path.split('.').collect();
    let is_section = segments.len() == 1 && root.get(path).is_some_and(toml::Value::is_table);
    let hint = match segments.as_slice() {
        ["editor"] | ["commands"] => Some(
            "the EDIT and COMMAND routes are not implemented in dictated yet, \
             so this section has no effect",
        ),
        ["whisper", key] if LEGACY_WHISPER_KEYS.contains(key) => {
            Some("a Python-era faster-whisper option with no whisper.cpp equivalent")
        }
        ["output", "typing_delay_ms" | "use_clipboard"] => {
            Some("injection is always clipboard-paste; [output] auto_type is the only switch")
        }
        _ => None,
    };
    let what = if is_section {
        format!("unknown section [{path}] ignored")
    } else {
        format!("unknown key '{path}' ignored")
    };
    match hint {
        Some(hint) => format!("{what} — {hint}"),
        None => what,
    }
}

/// Expand tildes in path fields.
fn finalize(mut config: Config) -> Config {
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
    config
}

/// Values that would crash or silently misbehave later.
fn validate(config: &Config) -> Vec<String> {
    let mut errors = Vec::new();
    let positive = |name: &str, v: f64, errors: &mut Vec<String>| {
        // `Duration::from_secs_f64` panics on negative or non-finite input, so
        // this would otherwise surface as a crash on the first dictation.
        if !v.is_finite() || v <= 0.0 {
            errors.push(format!(
                "{name} must be a positive number of seconds, got {v}"
            ));
        }
    };
    positive("grammar.timeout_s", config.grammar.timeout_s, &mut errors);
    positive("local.timeout_s", config.local.timeout_s, &mut errors);
    if config.upload.max_audio_seconds == 0 || config.upload.max_bytes == 0 {
        errors.push("upload.max_audio_seconds and upload.max_bytes must be non-zero".into());
    }
    let device = config.whisper.device.to_ascii_lowercase();
    if device != "cuda" && device != "cpu" {
        errors.push(format!(
            "whisper.device must be \"cuda\" or \"cpu\", got \"{}\"",
            config.whisper.device
        ));
    }
    errors
}

/// Suspicious-but-usable settings.
fn soft_checks(config: &Config) -> Vec<String> {
    let mut warnings = Vec::new();
    if !(0.0..=1.0).contains(&config.whisper.no_speech_threshold) {
        warnings.push(format!(
            "whisper.no_speech_threshold {} is outside 0.0..=1.0 and will be clamped",
            config.whisper.no_speech_threshold
        ));
    }
    if !config.audio.capture && config.hotkey.enabled {
        warnings.push(
            "hotkey.enabled with audio.capture = false: the hotkeys would start recordings \
             this daemon refuses to make"
                .into(),
        );
    }
    let path = Path::new(&config.whisper.model_path);
    if !path.exists() && dictate_stt::catalog_model(&config.whisper.model).is_none() {
        warnings.push(format!(
            "whisper.model '{}' is not in the catalog and {} does not exist; \
             transcription will fail",
            config.whisper.model,
            path.display()
        ));
    }
    warnings
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
        assert!(!config.history.privacy_mode);
        assert_eq!(config.history.retention_days, None);
        assert!(!config.history.import_python_db);
        assert!(config.timer.sound_enabled);
        assert!(!config.hotkey.enabled);
        assert_eq!(config.upload.max_audio_seconds, 600);
        assert!(config.hotkey.devices.is_empty());
        assert_eq!(config.hotkey.release_grace_ms, 45);
        assert_eq!(config.hotkey.double_tap_ms, 320);
    }

    #[test]
    fn test_toml_parsing_full() {
        let toml_str = r#"
[whisper]
model_path = "/tmp/test-model.bin"
device = "cpu"
no_speech_threshold = 0.8

[vad]
speech_threshold = 0.7
trailing_silence_ms = 1200

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
privacy_mode = true
retention_days = 30
import_python_db = true

[timer]
sound_enabled = false
sound_file = "/tmp/alarm.wav"

[hotkey]
enabled = true
devices = ["/dev/input/event7"]
hold_to_talk = [57]
toggle = [88]
cancel = [29, 56]
release_grace_ms = 50
double_tap_ms = 250
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.whisper.model_path, "/tmp/test-model.bin");
        assert_eq!(config.whisper.device, "cpu");
        assert!((config.whisper.no_speech_threshold - 0.8).abs() < f32::EPSILON);
        assert!((config.vad.speech_threshold - 0.7).abs() < f32::EPSILON);
        assert_eq!(config.vad.trailing_silence_ms, 1200);
        assert!(!config.grammar.enabled);
        assert_eq!(config.grammar.model, "test-model");
        assert_eq!(config.grammar.min_words, 5);
        assert!(!config.output.auto_type);
        assert!(!config.notifications.enabled);
        assert!(config.hotkey.enabled);
        assert_eq!(config.hotkey.devices, ["/dev/input/event7"]);
        assert_eq!(config.hotkey.hold_to_talk, [57]);
        assert_eq!(config.hotkey.toggle, [88]);
        assert_eq!(config.hotkey.cancel, [29, 56]);
        assert_eq!(config.hotkey.release_grace_ms, 50);
        assert_eq!(config.hotkey.double_tap_ms, 250);
        assert_eq!(config.notifications.timeout_ms, 5000);
        assert!(!config.history.enabled);
        assert_eq!(config.history.db_path, "/tmp/test.db");
        assert!(config.history.privacy_mode);
        assert_eq!(config.history.retention_days, Some(30));
        assert!(config.history.import_python_db);
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

    // --- Compatibility with the Python-era config shape -------------------

    const PYTHON_ERA: &str = include_str!("../tests/fixtures/python-era-config.toml");

    fn count(warnings: &[String], needle: &str) -> usize {
        warnings.iter().filter(|w| w.contains(needle)).count()
    }

    #[test]
    fn a_python_era_config_loads_with_its_meaning_preserved() {
        let (config, report) = parse_config(PYTHON_ERA).expect("must load");
        assert!(report.errors.is_empty(), "{:?}", report.errors);

        // The Hugging Face id maps onto the catalog entry the daemon can load.
        assert_eq!(config.whisper.model, "large-v3-turbo");
        // `[grammar] model` is honored, not replaced by the default.
        assert_eq!(config.grammar.model, "example-grammar:14b");
        assert_eq!(config.grammar.min_words, 3);
        // `[router]` is the Python-era `[local]`.
        assert_eq!(config.local.model, "example-local:7b");
        assert!((config.local.timeout_s - 120.0).abs() < f64::EPSILON);
        assert!(config.output.auto_type);
    }

    #[test]
    fn every_ignored_section_and_key_is_reported_exactly_once() {
        let (_, report) = parse_config(PYTHON_ERA).unwrap();
        let w = &report.warnings;
        for needle in [
            "unknown section [editor]",
            "unknown section [commands]",
            "unknown key 'output.typing_delay_ms'",
            "unknown key 'output.use_clipboard'",
            "unknown key 'whisper.assistant_model'",
            "unknown key 'whisper.compute_type'",
            "unknown key 'whisper.use_speculative_decoding'",
            "unknown key 'whisper.chunk_length_s'",
            "unknown key 'whisper.batch_size'",
        ] {
            assert_eq!(
                count(w, needle),
                1,
                "expected exactly one `{needle}` in {w:#?}"
            );
        }
        assert_eq!(count(w, "Hugging Face id"), 1, "{w:#?}");
        assert_eq!(count(w, "[router] is the Python-era name"), 1, "{w:#?}");
        // Sections are reported once as a section, not once per key inside.
        assert_eq!(count(w, "editor."), 0, "{w:#?}");
        assert_eq!(count(w, "commands."), 0, "{w:#?}");
    }

    #[test]
    fn a_warning_explains_what_to_do_not_just_what_is_wrong() {
        let (_, report) = parse_config(PYTHON_ERA).unwrap();
        let editor = report
            .warnings
            .iter()
            .find(|w| w.contains("[editor]"))
            .unwrap();
        assert!(editor.contains("not implemented"), "{editor}");
        let typing = report
            .warnings
            .iter()
            .find(|w| w.contains("typing_delay_ms"))
            .unwrap();
        assert!(typing.contains("auto_type"), "{typing}");
    }

    #[test]
    fn a_misspelled_key_is_not_silently_ignored() {
        let (config, report) = parse_config("[grammar]\nmodle = \"x\"\n").unwrap();
        assert_eq!(config.grammar.model, GrammarConfig::default().model);
        assert_eq!(count(&report.warnings, "unknown key 'grammar.modle'"), 1);
    }

    #[test]
    fn a_clean_current_config_produces_no_warnings() {
        let (_, report) = parse_config(
            "[whisper]\nmodel = \"large-v3-turbo\"\ndevice = \"cuda\"\n\n[grammar]\nmodel = \"m:1b\"\n",
        )
        .unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn an_explicit_local_section_beats_the_legacy_router_section() {
        let (config, report) = parse_config(
            "[router]\nollama_model = \"legacy:1b\"\n[local]\nmodel = \"current:2b\"\n",
        )
        .unwrap();
        assert_eq!(config.local.model, "current:2b");
        assert_eq!(count(&report.warnings, "wins"), 1, "{:?}", report.warnings);
    }

    #[test]
    fn a_hugging_face_id_without_a_catalog_twin_falls_back_loudly() {
        let (config, report) =
            parse_config("[whisper]\nmodel = \"distil-whisper/distil-large-v3\"\n").unwrap();
        assert_eq!(config.whisper.model, WhisperConfig::default().model);
        assert_eq!(count(&report.warnings, "no whisper.cpp catalog"), 1);
    }

    #[test]
    fn a_catalog_name_passes_through_untouched() {
        let (config, report) = parse_config("[whisper]\nmodel = \"tiny.en\"\n").unwrap();
        assert_eq!(config.whisper.model, "tiny.en");
        assert_eq!(count(&report.warnings, "Hugging Face"), 0);
    }

    #[test]
    fn values_that_would_crash_later_are_errors_now() {
        let (_, report) = parse_config("[grammar]\ntimeout_s = -1.0\n").unwrap();
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(report.errors[0].contains("grammar.timeout_s"));

        let (_, report) = parse_config("[whisper]\ndevice = \"gpu\"\n").unwrap();
        assert!(
            report.errors[0].contains("whisper.device"),
            "{:?}",
            report.errors
        );
    }

    #[test]
    fn a_wrong_type_or_bad_toml_is_a_hard_error() {
        assert!(parse_config("[grammar]\nmin_words = \"three\"\n").is_err());
        assert!(parse_config("[grammar\nmodel =").is_err());
    }

    #[test]
    fn recording_hotkeys_with_capture_disabled_is_flagged() {
        let (_, report) =
            parse_config("[audio]\ncapture = false\n[hotkey]\nenabled = true\n").unwrap();
        assert_eq!(count(&report.warnings, "audio.capture = false"), 1);
    }

    #[test]
    fn upload_limits_translate_into_protocol_limits() {
        let l = UploadConfig {
            max_audio_seconds: 30,
            max_bytes: 3_000_000,
        }
        .limits();
        assert_eq!(l.max_audio_ms, Some(30_000));
        // 3 MB of audio is 4 MB of base64, plus envelope slack.
        assert!(l.max_message_bytes >= 4_000_000);
        assert!(l.max_message_bytes < 4_100_000);
        // A pathological setting saturates instead of wrapping.
        let huge = UploadConfig {
            max_audio_seconds: u32::MAX,
            max_bytes: u64::MAX,
        }
        .limits();
        assert_eq!(huge.max_message_bytes, u32::MAX);
        assert_eq!(huge.max_audio_ms, Some(u32::MAX));
    }

    #[test]
    fn capture_defaults_on_and_can_be_switched_off() {
        assert!(Config::default().audio.capture);
        let (config, _) = parse_config("[audio]\ncapture = false\n").unwrap();
        assert!(!config.audio.capture);
    }

    #[test]
    fn load_config_fails_fast_on_an_invalid_value_and_names_the_file() {
        let dir = std::env::temp_dir().join(format!("dictate-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.toml");
        std::fs::write(&path, "[local]\ntimeout_s = 0\n").unwrap();
        let err = load_config(Some(&path)).unwrap_err().to_string();
        assert!(
            err.contains("bad.toml") && err.contains("local.timeout_s"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_file_reports_defaults_without_warnings() {
        let (config, report) =
            load_config_with_report(Some(Path::new("/tmp/nonexistent-dictate-config.toml")))
                .unwrap();
        assert!(!report.existed);
        assert!(report.warnings.is_empty());
        assert_eq!(config.whisper.device, "cuda");
    }
}
