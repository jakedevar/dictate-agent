use serde::{Deserialize, Serialize};

/// The requested injection method. S23 will select this from an active-app
/// profile; keeping it here makes that integration a data hand-off, not an
/// injection-backend rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InjectionPolicy {
    #[default]
    Paste,
    Type,
    Off,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct OutputConfig {
    pub auto_type: bool,
    /// Default policy when no future app profile supplies one.
    pub policy: InjectionPolicy,
    /// Bound direct-typing work so a pathological transcript cannot monopolize
    /// the input backend in one enormous request.
    pub type_chunk_chars: usize,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            auto_type: true,
            policy: InjectionPolicy::Paste,
            type_chunk_chars: 512,
        }
    }
}

// Deserialize/Default behavior for this type is exercised by the aggregate
// `Config` tests in dictate-core::config (test_default_values,
// test_toml_parsing_full, etc.) — not duplicated here.
