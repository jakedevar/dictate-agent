use dictate_proto::{AppCategory, AppContext, ContextInjection, ResolvedProfile, Tone};
use regex::{Regex, RegexBuilder};
use serde::{de::Error, Deserialize, Deserializer};

use crate::WindowInfo;

#[derive(Debug, Clone)]
pub struct ContextConfig {
    pub enabled: bool,
    pub profiles: Vec<Profile>,
}
impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            profiles: Vec::new(),
        }
    }
}

/// Regexes are compiled once, during configuration load.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    class: Option<Regex>,
    instance: Option<Regex>,
    title: Option<Regex>,
    category: Option<AppCategory>,
    options: ResolvedProfile,
}

#[derive(Deserialize)]
#[serde(default)]
struct RawConfig {
    enabled: bool,
    profiles: Vec<serde_json::Value>,
    #[serde(flatten)]
    unknown: std::collections::BTreeMap<String, serde::de::IgnoredAny>,
}
impl Default for RawConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            profiles: Vec::new(),
            unknown: Default::default(),
        }
    }
}
#[derive(Deserialize)]
struct RawProfile {
    name: String,
    #[serde(rename = "match")]
    matcher: RawMatcher,
    #[serde(default)]
    category: Option<AppCategory>,
    #[serde(default)]
    tone: Option<Tone>,
    #[serde(default)]
    llm_format: Option<bool>,
    #[serde(default)]
    inject: Option<ContextInjection>,
    #[serde(default)]
    spoken_punctuation: Option<bool>,
    #[serde(default)]
    spoken_line_breaks: Option<bool>,
    #[serde(flatten)]
    unknown: std::collections::BTreeMap<String, serde::de::IgnoredAny>,
}
#[derive(Deserialize, Default)]
struct RawMatcher {
    #[serde(default)]
    class: Option<String>,
    #[serde(default)]
    instance: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(flatten)]
    unknown: std::collections::BTreeMap<String, serde::de::IgnoredAny>,
}

impl<'de> Deserialize<'de> for ContextConfig {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = RawConfig::deserialize(d)?;
        for key in raw.unknown.keys() {
            tracing::warn!(section = "context", %key, "unknown configuration key");
        }
        let mut profiles = Vec::with_capacity(raw.profiles.len());
        for value in raw.profiles {
            // Decode each profile separately so even a type error (e.g. a
            // string instead of a bool) carries its profile's name.
            let name = value
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("<unnamed>");
            let p: RawProfile = serde_json::from_value(value.clone())
                .map_err(|e| D::Error::custom(format!("context profile {name:?}: {e}")))?;
            let invalid = |message: String| {
                D::Error::custom(format!("context profile {:?}: {message}", p.name))
            };
            if p.name.trim().is_empty() {
                return Err(invalid("name must not be empty".into()));
            }
            for key in p.unknown.keys().chain(p.matcher.unknown.keys()) {
                tracing::warn!(profile = %p.name, %key, "unknown context profile key");
            }
            if p.category.as_ref().is_some_and(|v| !v.is_known())
                || p.tone.as_ref().is_some_and(|v| !v.is_known())
                || p.inject.as_ref().is_some_and(|v| !v.is_known())
            {
                return Err(invalid("unknown category, tone or injection policy".into()));
            }
            if p.matcher.class.is_none()
                && p.matcher.instance.is_none()
                && p.matcher.title.is_none()
            {
                return Err(invalid(
                    "match must contain class, instance or title".into(),
                ));
            }
            let class = p
                .matcher
                .class
                .as_deref()
                .map(glob)
                .transpose()
                .map_err(|e| invalid(e.to_string()))?;
            let instance = p
                .matcher
                .instance
                .as_deref()
                .map(glob)
                .transpose()
                .map_err(|e| invalid(e.to_string()))?;
            let title = p
                .matcher
                .title
                .as_deref()
                .map(|s| RegexBuilder::new(s).size_limit(1 << 20).build())
                .transpose()
                .map_err(|e| invalid(e.to_string()))?;
            profiles.push(Profile {
                name: p.name,
                class,
                instance,
                title,
                category: p.category,
                options: ResolvedProfile {
                    tone: p.tone.unwrap_or_default(),
                    llm_format: p.llm_format,
                    inject: p.inject,
                    spoken_punctuation: p.spoken_punctuation,
                    spoken_line_breaks: p.spoken_line_breaks,
                    ..Default::default()
                },
            });
        }
        Ok(Self {
            enabled: raw.enabled,
            profiles,
        })
    }
}

/// Only `*` and `?` are special; all other characters are literal.
fn glob(pattern: &str) -> Result<Regex, regex::Error> {
    let mut regex = String::from("\\A(?:");
    for ch in pattern.chars() {
        match ch {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            c => regex.push_str(&regex::escape(&c.to_string())),
        }
    }
    regex.push_str(")\\z");
    RegexBuilder::new(&regex)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
}

fn matches(pattern: &Option<Regex>, value: Option<&str>) -> bool {
    pattern
        .as_ref()
        .is_none_or(|pattern| value.is_some_and(|value| pattern.is_match(value)))
}
impl ContextConfig {
    pub fn resolve(&self, window: Option<&WindowInfo>) -> ResolvedProfile {
        if !self.enabled {
            return ResolvedProfile::default();
        }
        let Some(window) = window else {
            return ResolvedProfile::default();
        };
        let Some(app) = window
            .class
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(window.instance.as_deref().filter(|s| !s.is_empty()))
            .or(window.process_name.as_deref())
        else {
            return ResolvedProfile::default();
        };
        let category = category_for(window);
        let profile = self.profiles.iter().find(|p| {
            matches(&p.class, window.class.as_deref())
                && matches(&p.instance, window.instance.as_deref())
                && matches(&p.title, window.title.as_deref())
        });
        let mut decision = profile.map(|p| p.options.clone()).unwrap_or_default();
        decision.context = Some(AppContext {
            app: app.to_lowercase(),
            title: window.title.clone(),
            category: profile.and_then(|p| p.category.clone()).unwrap_or(category),
            profile: profile.map(|p| p.name.clone()),
        });
        // Terminals keep spoken punctuation and line breaks off by default.
        // The LLM pass is *not* forced off: its terminal category policy
        // (`[format.llm.categories.terminal]`, verbatim) already limits it to
        // fillers, false starts, self-corrections and punctuation. The Ctrl+V
        // policy still inherits.
        if decision
            .context
            .as_ref()
            .is_some_and(|c| c.category == AppCategory::Terminal)
        {
            decision.spoken_punctuation.get_or_insert(false);
            decision.spoken_line_breaks.get_or_insert(false);
        }
        decision
    }
}

fn category_for(w: &WindowInfo) -> AppCategory {
    let class_category = w
        .class
        .as_deref()
        .map(class_category)
        .filter(|c| *c != AppCategory::Other)
        .or_else(|| w.instance.as_deref().map(class_category))
        .unwrap_or_default();
    if class_category == AppCategory::Browser
        && w.title.as_deref().is_some_and(|t| {
            let t = t.to_lowercase();
            t.contains("gmail") || t.contains("outlook")
        })
    {
        AppCategory::Email
    } else {
        class_category
    }
}
fn class_category(class: &str) -> AppCategory {
    match class.to_lowercase().as_str() {
        "com.mitchellh.ghostty"
        | "ghostty"
        | "kitty"
        | "alacritty"
        | "org.wezfurlong.wezterm"
        | "xterm"
        | "urxvt"
        | "konsole"
        | "gnome-terminal-server"
        | "st"
        | "foot" => AppCategory::Terminal,
        "code" | "code-oss" | "cursor" | "zed" | "emacs" | "neovide" | "gvim" => {
            AppCategory::Editor
        }
        c if c.starts_with("jetbrains-") => AppCategory::Editor,
        "google-chrome" | "chromium" | "firefox" | "brave-browser" => AppCategory::Browser,
        "slack" | "discord" | "signal" | "telegram-desktop" | "element" => AppCategory::Chat,
        "thunderbird" | "evolution" => AppCategory::Email,
        "libreoffice-writer" | "obsidian" | "notion" => AppCategory::Document,
        _ => AppCategory::Other,
    }
}
