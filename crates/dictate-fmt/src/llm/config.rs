//! `[format.llm]` configuration, and the deprecated `[grammar]` alias.
//!
//! The struct deserializes on its own (`#[serde(default)]` everywhere), so the
//! integrator can embed it as `FormatConfig::llm`. Loading through
//! [`LlmConfig::from_document`] additionally honours Jake's legacy `[grammar]`
//! keys and reports unknown keys — the contract forbids silently ignoring
//! either.

use std::sync::Once;

use dictate_proto::AppCategory;
use serde::Deserialize;
use tracing::warn;

/// Default model ladder, most preferred first. Chosen by the S21 benchmark
/// (see `thoughts/shared/research/2026-09-29-s21-llm-eval.md`): `gemma4:e4b`
/// is the only installed candidate inside the latency budget; `gemma4:12b`
/// scores comparably but costs ~2x the decode time, so it is a fallback, not
/// the default.
pub const DEFAULT_MODELS: &[&str] = &["gemma4:e4b", "gemma4:12b"];

/// How a category's text is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    /// Technical text (terminals, editors, coding-agent prompts): remove
    /// fillers, false starts and repeats, resolve self-corrections, fix
    /// punctuation and capitalization — nothing else. No rephrasing, no
    /// register change, no added words, no new line breaks.
    Verbatim,
    /// Prose: the verbatim clean-up plus light grammar repair and, where the
    /// category allows it, structure (numbered lists, paragraphs).
    Prose,
    /// Prose with email layout: a dictated greeting and sign-off go on their
    /// own lines. Never invents a greeting or sign-off.
    Email,
}

/// Per-category defaults. A profile (S23) or the session can still turn the
/// pass off; they cannot turn on a category disabled here except through an
/// explicit `SessionOptions.format_llm = Some(true)`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CategoryPolicy {
    /// Run the LLM pass for this category at all.
    pub enabled: bool,
    /// Prompt family.
    pub style: Style,
    /// Allow structure the speaker implied: numbered lists for enumerations
    /// and paragraph breaks for long prose. Ignored for `verbatim`.
    pub structure: bool,
}

impl Default for CategoryPolicy {
    fn default() -> Self {
        Self::prose(true)
    }
}

impl CategoryPolicy {
    const fn verbatim() -> Self {
        Self {
            enabled: true,
            style: Style::Verbatim,
            structure: false,
        }
    }
    const fn prose(structure: bool) -> Self {
        Self {
            enabled: true,
            style: Style::Prose,
            structure,
        }
    }
    const fn email() -> Self {
        Self {
            enabled: true,
            style: Style::Email,
            structure: true,
        }
    }

    /// Whether the output may contain line breaks the input did not have.
    #[must_use]
    pub fn allows_new_lines(&self) -> bool {
        self.style != Style::Verbatim && self.structure
    }
}

/// One [`CategoryPolicy`] per [`AppCategory`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CategoryPolicies {
    #[serde(deserialize_with = "deserialize_verbatim")]
    pub terminal: CategoryPolicy,
    #[serde(deserialize_with = "deserialize_verbatim")]
    pub editor: CategoryPolicy,
    #[serde(deserialize_with = "deserialize_structured")]
    pub browser: CategoryPolicy,
    #[serde(deserialize_with = "deserialize_chat")]
    pub chat: CategoryPolicy,
    #[serde(deserialize_with = "deserialize_email")]
    pub email: CategoryPolicy,
    #[serde(deserialize_with = "deserialize_structured")]
    pub document: CategoryPolicy,
    #[serde(deserialize_with = "deserialize_structured")]
    pub other: CategoryPolicy,
}

// A category table is a patch over that category's defaults. Deserializing
// it directly as CategoryPolicy would give every omitted field prose defaults.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CategoryPatch {
    enabled: Option<bool>,
    style: Option<Style>,
    structure: Option<bool>,
}

fn deserialize_policy<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
    mut policy: CategoryPolicy,
) -> Result<CategoryPolicy, D::Error> {
    let patch = CategoryPatch::deserialize(deserializer)?;
    if let Some(enabled) = patch.enabled {
        policy.enabled = enabled;
    }
    if let Some(style) = patch.style {
        policy.style = style;
    }
    if let Some(structure) = patch.structure {
        policy.structure = structure;
    }
    Ok(policy)
}

fn deserialize_verbatim<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<CategoryPolicy, D::Error> {
    deserialize_policy(d, CategoryPolicy::verbatim())
}
fn deserialize_structured<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<CategoryPolicy, D::Error> {
    deserialize_policy(d, CategoryPolicy::prose(true))
}
fn deserialize_chat<'de, D: serde::Deserializer<'de>>(d: D) -> Result<CategoryPolicy, D::Error> {
    deserialize_policy(d, CategoryPolicy::prose(false))
}
fn deserialize_email<'de, D: serde::Deserializer<'de>>(d: D) -> Result<CategoryPolicy, D::Error> {
    deserialize_policy(d, CategoryPolicy::email())
}

impl Default for CategoryPolicies {
    fn default() -> Self {
        Self {
            // Terminals carry coding-agent prompts and, occasionally, shell
            // input — a new line break there can submit or execute.
            terminal: CategoryPolicy::verbatim(),
            editor: CategoryPolicy::verbatim(),
            browser: CategoryPolicy::prose(true),
            // Chat messages are short; a numbered list is rarely what the
            // speaker meant and a stray newline can send the message.
            chat: CategoryPolicy::prose(false),
            email: CategoryPolicy::email(),
            document: CategoryPolicy::prose(true),
            other: CategoryPolicy::prose(true),
        }
    }
}

impl CategoryPolicies {
    /// The policy for `category`. Unknown categories (a newer peer's value)
    /// get `other`, the documented default.
    #[must_use]
    pub fn get(&self, category: &AppCategory) -> &CategoryPolicy {
        match category {
            AppCategory::Terminal => &self.terminal,
            AppCategory::Editor => &self.editor,
            AppCategory::Browser => &self.browser,
            AppCategory::Chat => &self.chat,
            AppCategory::Email => &self.email,
            AppCategory::Document => &self.document,
            _ => &self.other,
        }
    }
}

/// Per-request timeout: `base_ms + per_word_ms * words`, capped at `max_ms`.
///
/// Scaled because decode time is linear in output length (~6.5 ms/token for
/// `gemma4:e4b` on the RTX 5080, ~1.35 tokens/word), so one fixed timeout is
/// either too tight for long utterances or lets a stalled short one burn
/// seconds of the user's latency budget.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TimeoutPolicy {
    pub base_ms: u64,
    pub per_word_ms: u64,
    pub max_ms: u64,
}

impl Default for TimeoutPolicy {
    fn default() -> Self {
        Self {
            base_ms: 400,
            per_word_ms: 20,
            max_ms: 4000,
        }
    }
}

impl TimeoutPolicy {
    /// Timeout for a request carrying `words` words.
    #[must_use]
    pub fn for_words(&self, words: usize) -> std::time::Duration {
        let scaled = self
            .base_ms
            .saturating_add(self.per_word_ms.saturating_mul(words as u64));
        std::time::Duration::from_millis(scaled.min(self.max_ms))
    }
}

/// Long inputs: split at paragraph/sentence boundaries, format each chunk,
/// and above a hard limit skip the pass with an explicit reason.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ChunkPolicy {
    /// Inputs up to this many words go to the model in one request.
    pub max_single_words: usize,
    /// Target chunk size for longer inputs. Chunks end on sentence
    /// boundaries, so a chunk can exceed this by one sentence.
    pub chunk_words: usize,
    /// Chunks in flight at once. The default Ollama server
    /// (`OLLAMA_NUM_PARALLEL` unset) serializes requests, so >1 only helps
    /// when the server is configured for parallel slots.
    pub concurrency: usize,
    /// Above this many words the pass is skipped (`SkipReason::TooLong`):
    /// decode time grows linearly and the user would wait seconds.
    pub max_words: usize,
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        Self {
            max_single_words: 90,
            chunk_words: 60,
            concurrency: 1,
            max_words: 300,
        }
    }
}

/// `[format.llm]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    /// Master switch. Off by default in code (contract §4: no network
    /// dependency by default); `config.example.toml` turns it on.
    pub enabled: bool,
    /// Ollama base URL.
    pub host: String,
    /// Preference ladder, resolved against the installed models at startup
    /// and after a failure. The first installed entry is used.
    pub models: Vec<String>,
    /// Minimum words outside protected spans for the pass to run.
    pub min_words: usize,
    /// Ollama `keep_alive`: how long the model stays resident after a
    /// request (`"30m"`, `"1h"`, `"-1"` = forever, `"0"` = unload at once).
    pub keep_alive: String,
    /// Load the model when the daemon starts.
    pub warmup_on_start: bool,
    /// Load the model when recording starts, so it is resident when speech
    /// ends. Cheap when it is already loaded.
    pub warmup_on_record: bool,
    /// Sampling temperature. Clamped to <= 0.2.
    pub temperature: f32,
    /// Protect obviously technical tokens (slash commands, paths, URLs,
    /// identifiers) even when the caller did not mark them. Defence in depth
    /// behind the S20 protect stage.
    pub protect_fallback: bool,
    pub timeout: TimeoutPolicy,
    pub chunking: ChunkPolicy,
    pub categories: CategoryPolicies,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "http://localhost:11434".into(),
            models: DEFAULT_MODELS.iter().map(|m| (*m).to_string()).collect(),
            min_words: 3,
            keep_alive: "30m".into(),
            warmup_on_start: true,
            warmup_on_record: true,
            temperature: 0.1,
            protect_fallback: true,
            timeout: TimeoutPolicy::default(),
            chunking: ChunkPolicy::default(),
            categories: CategoryPolicies::default(),
        }
    }
}

/// Highest temperature the formatter will use. A formatter wants the most
/// likely rewrite, not a creative one.
pub const MAX_TEMPERATURE: f32 = 0.2;

/// Result of [`LlmConfig::from_document`].
#[derive(Debug, Clone)]
pub struct LlmConfigLoad {
    pub config: LlmConfig,
    /// Human-readable warnings (deprecated keys, unknown keys, clamped
    /// values). Already logged once; returned for `doctor`/tests.
    pub warnings: Vec<String>,
}

const KNOWN_TOP_KEYS: &[&str] = &[
    "enabled",
    "host",
    "models",
    "min_words",
    "keep_alive",
    "warmup_on_start",
    "warmup_on_record",
    "temperature",
    "protect_fallback",
    "timeout",
    "chunking",
    "categories",
];
const KNOWN_TIMEOUT_KEYS: &[&str] = &["base_ms", "per_word_ms", "max_ms"];
const KNOWN_CHUNK_KEYS: &[&str] = &[
    "max_single_words",
    "chunk_words",
    "concurrency",
    "max_words",
];
const KNOWN_CATEGORY_NAMES: &[&str] = &[
    "terminal", "editor", "browser", "chat", "email", "document", "other",
];
const KNOWN_CATEGORY_KEYS: &[&str] = &["enabled", "style", "structure"];
const LEGACY_GRAMMAR_KEYS: &[&str] = &["enabled", "host", "model", "timeout_s", "min_words"];

static LEGACY_WARNING: Once = Once::new();

impl LlmConfig {
    /// Resolve the effective LLM config from a whole parsed config file.
    ///
    /// - `[format.llm]` present: it is used; a `[grammar]` section alongside
    ///   it is ignored with a warning.
    /// - only `[grammar]` present (Jake's Python-era file): its keys map onto
    ///   this struct as a deprecated alias, with a one-time warning:
    ///   `enabled` → `enabled`, `host` → `host`, `min_words` → `min_words`,
    ///   `timeout_s` → `timeout.max_ms` (it was a hard cap), and `model` is
    ///   put *first* in the default ladder — honoured when installed, and when
    ///   it is not (today's `qwen3:14b`), the ladder resolves to an installed
    ///   model and the miss is reported loudly instead of failing every
    ///   dictation.
    /// - neither: defaults.
    ///
    /// # Errors
    ///
    /// If `[format.llm]` or `[grammar]` has a value of the wrong type.
    pub fn from_document(root: &toml::Table) -> Result<LlmConfigLoad, toml::de::Error> {
        let mut warnings = Vec::new();
        let format_llm = root
            .get("format")
            .and_then(toml::Value::as_table)
            .and_then(|f| f.get("llm"))
            .and_then(toml::Value::as_table);
        let grammar = root.get("grammar").and_then(toml::Value::as_table);

        let mut config = match (format_llm, grammar) {
            (Some(table), legacy) => {
                if legacy.is_some() {
                    warnings.push(
                        "[grammar] is ignored because [format.llm] is present; remove [grammar]"
                            .to_string(),
                    );
                }
                collect_unknown_keys(table, &mut warnings);
                toml::Value::Table(table.clone()).try_into::<LlmConfig>()?
            }
            (None, Some(table)) => {
                let legacy: LegacyGrammar = toml::Value::Table(table.clone()).try_into()?;
                for key in table.keys() {
                    if !LEGACY_GRAMMAR_KEYS.contains(&key.as_str()) {
                        warnings.push(format!("unknown key [grammar].{key} ignored"));
                    }
                }
                let config = legacy.into_config();
                LEGACY_WARNING.call_once(|| {
                    warn!(
                        "config: [grammar] is deprecated; move it to [format.llm] \
                         (enabled={}, models={:?}, min_words={}, timeout.max_ms={})",
                        config.enabled, config.models, config.min_words, config.timeout.max_ms
                    );
                });
                warnings.push("[grammar] is deprecated; use [format.llm]".to_string());
                config
            }
            (None, None) => LlmConfig::default(),
        };

        config.sanitize(&mut warnings);
        for w in &warnings {
            if !w.starts_with("[grammar] is deprecated") {
                warn!("config: {w}");
            }
        }
        Ok(LlmConfigLoad { config, warnings })
    }

    /// Clamp values that would make the pass unsafe or nonsensical.
    fn sanitize(&mut self, warnings: &mut Vec<String>) {
        if !(0.0..=MAX_TEMPERATURE).contains(&self.temperature) {
            warnings.push(format!(
                "[format.llm].temperature {} clamped to 0.0..={MAX_TEMPERATURE}",
                self.temperature
            ));
            self.temperature = self.temperature.clamp(0.0, MAX_TEMPERATURE);
        }
        if !is_valid_keep_alive(&self.keep_alive) {
            warnings.push(format!(
                "[format.llm].keep_alive {:?} is not an Ollama duration; using \"30m\"",
                self.keep_alive
            ));
            self.keep_alive = "30m".into();
        }
        self.models.retain(|m| !m.trim().is_empty());
        if self.models.is_empty() {
            warnings.push("[format.llm].models is empty; using the default ladder".into());
            self.models = DEFAULT_MODELS.iter().map(|m| (*m).to_string()).collect();
        }
        if self.chunking.chunk_words == 0 {
            self.chunking.chunk_words = ChunkPolicy::default().chunk_words;
        }
        if self.chunking.concurrency == 0 {
            self.chunking.concurrency = 1;
        }
        if self.timeout.max_ms == 0 {
            warnings.push("[format.llm].timeout.max_ms = 0 would fail every request".into());
            self.timeout.max_ms = TimeoutPolicy::default().max_ms;
        }
    }
}

/// `keep_alive` accepts what Ollama accepts: an integer (seconds; `-1`
/// forever, `0` unload) or an integer with an `ms`/`s`/`m`/`h` unit.
fn is_valid_keep_alive(s: &str) -> bool {
    let s = s.trim();
    let digits = s.strip_prefix('-').unwrap_or(s);
    let unit_start = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    unit_start > 0 && matches!(&digits[unit_start..], "" | "ms" | "s" | "m" | "h")
}

fn collect_unknown_keys(table: &toml::Table, warnings: &mut Vec<String>) {
    for (key, value) in table {
        if !KNOWN_TOP_KEYS.contains(&key.as_str()) {
            warnings.push(format!("unknown key [format.llm].{key} ignored"));
            continue;
        }
        let Some(sub) = value.as_table() else {
            continue;
        };
        let known: &[&str] = match key.as_str() {
            "timeout" => KNOWN_TIMEOUT_KEYS,
            "chunking" => KNOWN_CHUNK_KEYS,
            "categories" => {
                for (name, policy) in sub {
                    if !KNOWN_CATEGORY_NAMES.contains(&name.as_str()) {
                        warnings.push(format!(
                            "unknown category [format.llm.categories].{name} ignored"
                        ));
                    } else if let Some(policy) = policy.as_table() {
                        for k in policy.keys() {
                            if !KNOWN_CATEGORY_KEYS.contains(&k.as_str()) {
                                warnings.push(format!(
                                    "unknown key [format.llm.categories.{name}].{k} ignored"
                                ));
                            }
                        }
                    }
                }
                continue;
            }
            _ => continue,
        };
        for k in sub.keys() {
            if !known.contains(&k.as_str()) {
                warnings.push(format!("unknown key [format.llm.{key}].{k} ignored"));
            }
        }
    }
}

/// The Python-era `[grammar]` section, every key optional so presence is
/// visible.
#[derive(Debug, Default, Deserialize)]
struct LegacyGrammar {
    enabled: Option<bool>,
    host: Option<String>,
    model: Option<String>,
    timeout_s: Option<f64>,
    min_words: Option<usize>,
}

impl LegacyGrammar {
    fn into_config(self) -> LlmConfig {
        // The historical [grammar] table enabled the pass unless explicitly
        // opted out; the new default (no table) remains disabled.
        let mut config = LlmConfig {
            enabled: self.enabled.unwrap_or(true),
            ..LlmConfig::default()
        };
        if let Some(host) = self.host {
            config.host = host;
        }
        if let Some(min_words) = self.min_words {
            config.min_words = min_words;
        }
        if let Some(timeout_s) = self.timeout_s {
            if timeout_s.is_finite() && timeout_s > 0.0 {
                config.timeout.max_ms = (timeout_s * 1000.0).round() as u64;
            }
        }
        if let Some(model) = self.model {
            let model = model.trim().to_string();
            if !model.is_empty() {
                config.models.retain(|m| *m != model);
                config.models.insert(0, model);
            }
        }
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(toml_src: &str) -> LlmConfigLoad {
        let root: toml::Table = toml::from_str(toml_src).unwrap();
        LlmConfig::from_document(&root).unwrap()
    }

    #[test]
    fn partial_category_tables_preserve_each_categorys_defaults() {
        for category in [
            "terminal", "editor", "browser", "chat", "email", "document", "other",
        ] {
            let text = format!("[format.llm.categories.{category}]\nenabled = false\n");
            let loaded = load(&text).config;
            let raw: crate::FormatConfig =
                toml::from_str(&format!("[llm.categories.{category}]\nenabled = false\n")).unwrap();
            let mut expected = CategoryPolicies::default()
                .get(&AppCategory::from(category))
                .clone();
            expected.enabled = false;
            assert_eq!(
                loaded.categories.get(&AppCategory::from(category)),
                &expected
            );
            assert_eq!(
                raw.llm.categories.get(&AppCategory::from(category)),
                &expected
            );
        }
    }

    #[test]
    fn defaults_are_conservative() {
        let c = LlmConfig::default();
        assert!(!c.enabled, "no network dependency by default (contract §4)");
        assert_eq!(c.models, ["gemma4:e4b", "gemma4:12b"]);
        assert!(c.temperature <= MAX_TEMPERATURE);
        assert!(c.protect_fallback);
        assert_eq!(c.categories.terminal.style, Style::Verbatim);
        assert!(!c.categories.terminal.allows_new_lines());
        assert!(!c.categories.editor.allows_new_lines());
        assert!(!c.categories.chat.allows_new_lines());
        assert!(c.categories.document.allows_new_lines());
        assert_eq!(c.categories.email.style, Style::Email);
    }

    #[test]
    fn empty_document_is_defaults() {
        let l = load("");
        assert_eq!(l.config, LlmConfig::default());
        assert!(l.warnings.is_empty());
    }

    #[test]
    fn format_llm_section_parses_every_key() {
        let l = load(
            r#"
[format.llm]
enabled = true
host = "http://127.0.0.1:9999"
models = ["a:1b", "b:2b"]
min_words = 5
keep_alive = "1h"
warmup_on_start = false
warmup_on_record = false
temperature = 0.0
protect_fallback = false
[format.llm.timeout]
base_ms = 100
per_word_ms = 10
max_ms = 900
[format.llm.chunking]
max_single_words = 40
chunk_words = 20
concurrency = 2
max_words = 100
[format.llm.categories.terminal]
style = "prose"
structure = true
[format.llm.categories.chat]
enabled = false
"#,
        );
        let c = l.config;
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert!(c.enabled);
        assert_eq!(c.host, "http://127.0.0.1:9999");
        assert_eq!(c.models, ["a:1b", "b:2b"]);
        assert_eq!(c.min_words, 5);
        assert_eq!(c.keep_alive, "1h");
        assert!(!c.warmup_on_start && !c.warmup_on_record && !c.protect_fallback);
        assert_eq!(c.temperature, 0.0);
        assert_eq!(
            c.timeout,
            TimeoutPolicy {
                base_ms: 100,
                per_word_ms: 10,
                max_ms: 900
            }
        );
        assert_eq!(c.chunking.concurrency, 2);
        assert_eq!(c.categories.terminal.style, Style::Prose);
        assert!(
            c.categories.terminal.enabled,
            "omitted key keeps its default"
        );
        assert!(!c.categories.chat.enabled);
        // An unmentioned category keeps its default policy.
        assert_eq!(c.categories.editor, CategoryPolicy::verbatim());
    }

    #[test]
    fn legacy_grammar_section_is_an_alias_with_the_model_first_in_the_ladder() {
        // Jake's real legacy keys (values are the documented ones; nothing
        // personal).
        let l = load(
            r#"
[grammar]
enabled = true
model = "qwen3:14b"
timeout_s = 10.0
min_words = 3
"#,
        );
        let c = &l.config;
        assert!(c.enabled);
        assert_eq!(c.models, ["qwen3:14b", "gemma4:e4b", "gemma4:12b"]);
        assert_eq!(c.min_words, 3);
        assert_eq!(c.timeout.max_ms, 10_000);
        assert!(l.warnings.iter().any(|w| w.contains("deprecated")));
    }

    #[test]
    fn legacy_model_already_in_the_ladder_is_not_duplicated() {
        let l = load("[grammar]\nmodel = \"gemma4:12b\"\n");
        assert_eq!(l.config.models, ["gemma4:12b", "gemma4:e4b"]);
    }

    #[test]
    fn legacy_table_without_enabled_keeps_the_historical_opt_in() {
        for text in ["[grammar]\n", "[grammar]\nmodel = \"qwen3:14b\"\n"] {
            assert!(load(text).config.enabled);
        }
        assert!(!load("").config.enabled);
    }

    #[test]
    fn legacy_disabled_stays_disabled() {
        let l = load("[grammar]\nenabled = false\n");
        assert!(!l.config.enabled);
    }

    #[test]
    fn format_llm_wins_over_grammar_and_says_so() {
        let l = load(
            r#"
[grammar]
enabled = false
model = "qwen3:14b"
[format.llm]
enabled = true
"#,
        );
        assert!(l.config.enabled);
        assert_eq!(l.config.models, ["gemma4:e4b", "gemma4:12b"]);
        assert!(l.warnings.iter().any(|w| w.contains("ignored")));
    }

    #[test]
    fn unknown_keys_are_reported_not_silently_ignored() {
        let l = load(
            r#"
[format.llm]
enabeld = true
[format.llm.timeout]
max = 5
[format.llm.categories.spreadsheet]
enabled = true
[format.llm.categories.chat]
tone = "casual"
"#,
        );
        let joined = l.warnings.join("\n");
        assert!(joined.contains("[format.llm].enabeld"), "{joined}");
        assert!(joined.contains("[format.llm.timeout].max"), "{joined}");
        assert!(joined.contains("categories].spreadsheet"), "{joined}");
        assert!(
            joined.contains("[format.llm.categories.chat].tone"),
            "{joined}"
        );
    }

    #[test]
    fn unsafe_values_are_clamped_with_a_warning() {
        let l = load(
            r#"
[format.llm]
temperature = 0.9
keep_alive = "forever"
models = []
[format.llm.timeout]
max_ms = 0
"#,
        );
        let c = &l.config;
        assert_eq!(c.temperature, MAX_TEMPERATURE);
        assert_eq!(c.keep_alive, "30m");
        assert_eq!(c.models, ["gemma4:e4b", "gemma4:12b"]);
        assert_eq!(c.timeout.max_ms, TimeoutPolicy::default().max_ms);
        assert_eq!(l.warnings.len(), 4, "{:?}", l.warnings);
    }

    #[test]
    fn the_documented_example_config_parses_without_unknown_keys() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/config.example.toml"
        ))
        .unwrap();
        let root: toml::Table = toml::from_str(&text).unwrap();
        let l = LlmConfig::from_document(&root).unwrap();
        // The legacy pass is gone and the example no longer carries a
        // [grammar] section: every key is known, nothing is deprecated.
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        let documented = LlmConfig {
            enabled: true,
            ..LlmConfig::default()
        };
        assert_eq!(
            l.config, documented,
            "example values must match the defaults they document"
        );
    }

    #[test]
    fn wrong_types_are_errors_not_defaults() {
        let root: toml::Table = toml::from_str("[format.llm]\nenabled = \"yes\"\n").unwrap();
        assert!(LlmConfig::from_document(&root).is_err());
    }

    #[test]
    fn keep_alive_validation() {
        for ok in ["30m", "1h", "-1", "0", "300", "45s", "500ms"] {
            assert!(is_valid_keep_alive(ok), "{ok}");
        }
        for bad in ["", "m", "forever", "1d", "-", "1.5h"] {
            assert!(!is_valid_keep_alive(bad), "{bad}");
        }
    }

    #[test]
    fn timeout_scales_with_words_and_is_capped() {
        let t = TimeoutPolicy {
            base_ms: 400,
            per_word_ms: 20,
            max_ms: 1500,
        };
        assert_eq!(t.for_words(0).as_millis(), 400);
        assert_eq!(t.for_words(17).as_millis(), 740);
        assert_eq!(t.for_words(53).as_millis(), 1460);
        assert_eq!(t.for_words(500).as_millis(), 1500);
    }

    #[test]
    fn unknown_category_values_fall_back_to_other() {
        let p = CategoryPolicies::default();
        assert_eq!(p.get(&AppCategory::Unknown("spreadsheet".into())), &p.other);
    }
}
