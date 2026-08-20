//! Records exchanged by the CRUD and query commands: configuration,
//! dictionary, snippets, and history.

use serde::{Deserialize, Serialize};

use crate::state::{Route, SessionId};
use crate::timings::StageTimings;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// One configuration setting, addressed by dotted path.
///
/// # Why the config schema is not modeled here
///
/// It would be natural to mirror `dictate-core::config::Config` as a struct in
/// this crate. That is a trap: every new config key any later slice adds — S10's
/// pre-roll buffer, S11's VAD thresholds, S22's dictionary paths — would become
/// a protocol change, and the UI would need a rebuild to expose a setting the
/// daemon already supports.
///
/// Addressing settings by dotted path with an opaque [`serde_json::Value`]
/// keeps configuration evolution entirely inside `dictate-core`. The cost is
/// that the protocol cannot type-check a setting; validation is the daemon's
/// job, reported as [`ErrorCode::ConfigInvalid`](crate::ErrorCode::ConfigInvalid).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigEntry {
    /// Dotted path into the configuration tree, e.g. `"whisper.model"`.
    pub path: String,
    /// The value at that path.
    pub value: serde_json::Value,
}

impl ConfigEntry {
    /// Construct a config entry.
    pub fn new(path: impl Into<String>, value: serde_json::Value) -> Self {
        Self {
            path: path.into(),
            value,
        }
    }
}

/// A configuration read result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigSnapshot {
    /// The requested subtree, or the whole configuration when no path was
    /// given.
    pub values: serde_json::Value,
    /// The path this snapshot is rooted at; absent means the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Paths that were changed by the request that produced this snapshot.
    /// Empty for a plain read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied: Vec<String>,
    /// Paths that require a daemon restart before they take effect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restart_required: Vec<String>,
}

// ---------------------------------------------------------------------------
// Dictionary
// ---------------------------------------------------------------------------

open_str_enum! {
    /// How a dictionary entry came to exist.
    pub enum EntrySource {
        /// Added deliberately by the user.
        Manual => "manual",
        /// Inferred by the auto-learn pass from repeated corrections (S24).
        AutoLearned => "auto_learned",
        /// Shipped with the daemon.
        Builtin => "builtin",
        /// Received from another machine via the sync surface.
        Synced => "synced",
    }
    default = Manual;
}

/// A personal-dictionary entry: a term the recognizer should get right.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DictionaryEntry {
    /// Server-assigned identifier. Absent when creating a new entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,

    /// The canonical spelling to produce, e.g. `"Kubernetes"`.
    pub phrase: String,

    /// Transcriptions that should be rewritten to `phrase`, e.g.
    /// `["kubernetties", "cube ernetties"]`. Empty means the entry only biases
    /// recognition toward `phrase` without rewriting anything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sounds_like: Vec<String>,

    /// Whether matching against `sounds_like` respects case.
    #[serde(default)]
    pub case_sensitive: bool,

    /// Whether this entry is applied.
    #[serde(default = "crate::records::default_true")]
    pub enabled: bool,

    /// Where the entry came from.
    #[serde(default)]
    pub source: EntrySource,

    /// Times this entry has matched, for auto-learn scoring. Server-maintained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_count: Option<u64>,
}

impl DictionaryEntry {
    /// A new manual entry for `phrase`.
    pub fn new(phrase: impl Into<String>) -> Self {
        Self {
            id: None,
            phrase: phrase.into(),
            sounds_like: Vec::new(),
            case_sensitive: false,
            enabled: true,
            source: EntrySource::Manual,
            hit_count: None,
        }
    }
}

/// A text snippet expanded from a spoken trigger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snippet {
    /// Server-assigned identifier. Absent when creating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// The spoken phrase that triggers expansion, e.g. `"my address"`.
    pub trigger: String,
    /// The text substituted in its place.
    pub expansion: String,
    /// Whether this snippet is active.
    #[serde(default = "crate::records::default_true")]
    pub enabled: bool,
    /// Optional grouping label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

impl Snippet {
    /// A new enabled snippet.
    pub fn new(trigger: impl Into<String>, expansion: impl Into<String>) -> Self {
        Self {
            id: None,
            trigger: trigger.into(),
            expansion: expansion.into(),
            enabled: true,
            category: None,
        }
    }
}

pub(crate) fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

open_str_enum! {
    /// Sort direction for history results.
    pub enum SortOrder {
        /// Newest first. The default.
        Descending => "desc",
        /// Oldest first.
        Ascending => "asc",
    }
    default = Descending;
}

/// Filters for a history query.
///
/// Every field is optional; an empty query returns the most recent page.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HistoryQuery {
    /// Full-text search over transcripts, matched against the FTS5 index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Restrict to one route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<Route>,
    /// Restrict to one session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Inclusive lower bound, milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// Exclusive upper bound, milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<u64>,
    /// Maximum rows to return. The server clamps this to its own ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Rows to skip, for paging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    /// Sort direction.
    #[serde(default)]
    pub order: SortOrder,
    /// Return only sessions that ended in an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors_only: Option<bool>,
}

/// One recorded dictation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Row identifier.
    pub id: i64,
    /// The session that produced this row.
    pub session_id: SessionId,
    /// When the session started, in milliseconds since the Unix epoch.
    pub ts_ms: u64,

    /// The transcript as delivered, after all formatting.
    ///
    /// Absent when the session ran in privacy mode or failed before producing
    /// text — distinct from an empty string, which means the user said nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// The raw speech-to-text output before corrections and formatting. Kept
    /// separate so the formatting layers can be evaluated against ground truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,

    /// Where the transcript was dispatched.
    #[serde(default)]
    pub route: Route,

    /// Per-stage latency for this session. This is the field the ≤1.0s p50
    /// budget is measured from.
    #[serde(default)]
    pub timings: StageTimings,

    /// Word count of the delivered text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_count: Option<u32>,

    /// Effective words per minute of speech, for the analytics cards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wpm: Option<f64>,

    /// The failure, if the session ended in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::error::ProtoError>,

    /// The application that had focus, when context detection is available
    /// (S23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
}

/// A page of history results.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HistoryPage {
    /// The rows.
    pub items: Vec<HistoryEntry>,
    /// Total rows matching the query, ignoring paging. Optional because
    /// computing it can cost a second scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Offset to pass to fetch the next page; absent when this is the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
}

/// Aggregate history metrics for the CLI and dashboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryAnalytics {
    /// Weighted words per minute across completed dictations with audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overall_wpm: Option<f64>,
    /// Words dictated since the start of the current UTC day.
    pub words_today: u64,
    /// Completed words grouped by UTC day, oldest first.
    #[serde(default)]
    pub words_by_day: Vec<DailyWords>,
    /// Consecutive active days ending today.
    pub current_streak_days: u32,
    /// Longest consecutive active-day run in retained history.
    pub longest_streak_days: u32,
}

/// One UTC day's dictated-word total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyWords {
    /// ISO-8601 calendar date (`YYYY-MM-DD`).
    pub day: String,
    /// Words completed that day.
    pub words: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_entry_defaults_to_enabled_manual() {
        let e: DictionaryEntry = serde_json::from_str(r#"{"phrase":"Kubernetes"}"#).unwrap();
        assert!(e.enabled, "an omitted `enabled` must not disable the entry");
        assert_eq!(e.source, EntrySource::Manual);
        assert_eq!(e.id, None);
        assert!(e.sounds_like.is_empty());
        assert!(!e.case_sensitive);
    }

    #[test]
    fn snippet_defaults_to_enabled() {
        let s: Snippet = serde_json::from_str(r#"{"trigger":"sig","expansion":"— Jake"}"#).unwrap();
        assert!(s.enabled);
        assert_eq!(s.category, None);
    }

    #[test]
    fn new_entries_omit_server_assigned_fields() {
        let json = serde_json::to_string(&DictionaryEntry::new("Kubernetes")).unwrap();
        assert!(!json.contains("\"id\""), "{json}");
        assert!(!json.contains("hit_count"), "{json}");
        assert!(!json.contains("sounds_like"), "{json}");
    }

    #[test]
    fn empty_history_query_serializes_to_just_its_default_order() {
        let json = serde_json::to_string(&HistoryQuery::default()).unwrap();
        assert_eq!(json, r#"{"order":"desc"}"#);
    }

    #[test]
    fn history_query_round_trips_with_filters() {
        let q = HistoryQuery {
            text: Some("kubernetes".into()),
            route: Some(Route::Type),
            since_ms: Some(1_700_000_000_000),
            limit: Some(50),
            order: SortOrder::Ascending,
            ..Default::default()
        };
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(serde_json::from_str::<HistoryQuery>(&json).unwrap(), q);
    }

    /// Privacy mode and "the user said nothing" must not look alike.
    #[test]
    fn absent_text_is_distinct_from_empty_text() {
        let redacted: HistoryEntry =
            serde_json::from_str(r#"{"id":1,"session_id":"s","ts_ms":0}"#).unwrap();
        assert_eq!(redacted.text, None);

        let silent: HistoryEntry =
            serde_json::from_str(r#"{"id":1,"session_id":"s","ts_ms":0,"text":""}"#).unwrap();
        assert_eq!(silent.text, Some(String::new()));
        assert_ne!(redacted.text, silent.text);
    }

    #[test]
    fn history_entry_defaults_route_and_timings() {
        let e: HistoryEntry =
            serde_json::from_str(r#"{"id":7,"session_id":"abc","ts_ms":1000}"#).unwrap();
        assert_eq!(e.route, Route::Type);
        assert_eq!(e.timings, StageTimings::default());
        assert!(e.error.is_none());
    }

    #[test]
    fn config_entry_carries_arbitrary_values() {
        let e = ConfigEntry::new("whisper.model", serde_json::json!("large-v3-turbo"));
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(
            json,
            r#"{"path":"whisper.model","value":"large-v3-turbo"}"#
        );

        // A nested table is equally expressible, so a future config shape needs
        // no protocol change.
        let nested = ConfigEntry::new("vad", serde_json::json!({"threshold": 0.5}));
        assert_eq!(serde_json::from_str::<ConfigEntry>(
            &serde_json::to_string(&nested).unwrap()).unwrap(), nested);
    }

    #[test]
    fn history_page_defaults_are_empty() {
        let p: HistoryPage = serde_json::from_str(r#"{"items":[]}"#).unwrap();
        assert!(p.items.is_empty());
        assert_eq!(p.total, None);
        assert_eq!(p.next_offset, None);
    }
}
