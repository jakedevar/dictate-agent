use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::time::Instant;
use tracing::{error, info};

const SCHEMA_VERSION: i32 = 1;
const DEFAULT_DB_DIR: &str = "dictate-agent";

pub struct HistoryStore {
    conn: Connection,
    session_id: String,
    enabled: bool,
}

/// Mutable interaction builder — populated field-by-field across the pipeline.
/// Mirrors Python's Interaction dataclass at history.py:69-113.
#[derive(Default)]
pub struct Interaction {
    pub session_id: String,
    pub timestamp: String,
    pub start_time: Option<Instant>,

    // Audio
    pub audio_duration_s: Option<f64>,

    // Transcription
    pub raw_transcription: Option<String>,
    pub corrected_transcription: Option<String>,
    pub transcription_duration_s: Option<f64>,

    // Grammar
    pub grammar_input: Option<String>,
    pub grammar_output: Option<String>,
    pub grammar_changed: bool,
    pub grammar_error: Option<String>,
    pub grammar_duration_s: Option<f64>,

    // Routing
    pub route_type: Option<String>,
    pub route_model: Option<String>,
    pub route_trigger: Option<String>,
    pub route_confidence: Option<f64>,

    // Execution
    pub prompt_sent: Option<String>,
    pub response_text: Option<String>,
    pub execution_model: Option<String>,
    pub execution_duration_s: Option<f64>,
    pub execution_success: Option<bool>,
    pub execution_error: Option<String>,

    // Output
    pub output_typed: bool,
    pub output_char_count: Option<usize>,

    // Pipeline
    pub completed: bool,
    pub error_summary: Option<String>,
}

impl HistoryStore {
    pub fn new(config: &crate::config::HistoryConfig) -> Result<Self> {
        if !config.enabled {
            // Create a disabled store with an in-memory connection
            let conn = Connection::open_in_memory()?;
            return Ok(Self {
                conn,
                session_id: String::new(),
                enabled: false,
            });
        }

        let db_path = if config.db_path.is_empty() {
            default_db_path()
        } else {
            crate::config::expand_tilde(&config.db_path)
        };

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL")?;

        // Create tables — same schema as Python history.py:18-66
        conn.execute_batch(include_str!("../../../sql/schema.sql"))?;

        // Insert schema version if absent
        let count: i32 = conn.query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))?;
        if count == 0 {
            conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                [SCHEMA_VERSION],
            )?;
        }

        let session_id = uuid::Uuid::new_v4().to_string()[..12].to_string();
        info!(
            "History store opened at {} (session {})",
            db_path.display(),
            session_id
        );

        Ok(Self {
            conn,
            session_id,
            enabled: true,
        })
    }

    /// The underlying connection, for the read path in [`crate::query`].
    ///
    /// Read-only by convention: writes go through [`HistoryStore::commit`] so
    /// the insert statement and the schema stay in one place.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Whether this store is persisting anything.
    ///
    /// A disabled store is backed by an in-memory database, so queries against
    /// it succeed and return nothing — which is the correct answer for a user
    /// who has turned history off, and better than an error the UI would have
    /// to special-case.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Run a [`dictate_proto::HistoryQuery`] against the log.
    ///
    /// # Errors
    ///
    /// Propagates SQLite failures.
    pub fn query(
        &self,
        q: &dictate_proto::HistoryQuery,
    ) -> Result<dictate_proto::HistoryPage> {
        crate::query::query(&self.conn, q)
    }

    pub fn begin(&self) -> Interaction {
        Interaction {
            session_id: self.session_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            start_time: Some(Instant::now()),
            ..Interaction::default()
        }
    }

    pub fn commit(&self, interaction: &Interaction) {
        if !self.enabled {
            return;
        }
        let total_duration = interaction
            .start_time
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        if let Err(e) = self.insert(interaction, total_duration) {
            error!("Failed to commit interaction: {}", e);
        }
    }

    fn insert(&self, i: &Interaction, total_duration: f64) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO interactions (
                session_id, timestamp, audio_duration_s,
                raw_transcription, corrected_transcription, transcription_duration_s,
                grammar_input, grammar_output, grammar_changed, grammar_error, grammar_duration_s,
                route_type, route_model, route_trigger, route_confidence,
                prompt_sent, response_text, execution_model, execution_duration_s,
                execution_success, execution_error,
                output_typed, output_char_count,
                total_duration_s, completed, error_summary
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21,
                ?22, ?23, ?24, ?25, ?26
            )",
            params![
                i.session_id,
                i.timestamp,
                i.audio_duration_s,
                i.raw_transcription,
                i.corrected_transcription,
                i.transcription_duration_s,
                i.grammar_input,
                i.grammar_output,
                i.grammar_changed as i32,
                i.grammar_error,
                i.grammar_duration_s,
                i.route_type,
                i.route_model,
                i.route_trigger,
                i.route_confidence,
                i.prompt_sent,
                i.response_text,
                i.execution_model,
                i.execution_duration_s,
                i.execution_success.map(|b| b as i32),
                i.execution_error,
                i.output_typed as i32,
                i.output_char_count.map(|c| c as i64),
                total_duration,
                i.completed as i32,
                i.error_summary,
            ],
        )?;
        Ok(())
    }
}

fn default_db_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".local/share"))
                .unwrap_or_else(|_| PathBuf::from("/tmp"))
        });
    base.join(DEFAULT_DB_DIR).join("history.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_history_store_create() {
        let config = crate::config::HistoryConfig {
            enabled: true,
            db_path: "/tmp/dictate-agent-test-history.db".into(),
            max_response_length: 10000,
        };
        let store = HistoryStore::new(&config).unwrap();
        assert!(store.enabled);
        assert!(!store.session_id.is_empty());

        // Cleanup
        let _ = std::fs::remove_file("/tmp/dictate-agent-test-history.db");
    }

    #[test]
    fn test_history_store_disabled() {
        let config = crate::config::HistoryConfig {
            enabled: false,
            db_path: String::new(),
            max_response_length: 10000,
        };
        let store = HistoryStore::new(&config).unwrap();
        assert!(!store.enabled);
    }

    #[test]
    fn test_interaction_commit() {
        let config = crate::config::HistoryConfig {
            enabled: true,
            db_path: "/tmp/dictate-agent-test-history-commit.db".into(),
            max_response_length: 10000,
        };
        let store = HistoryStore::new(&config).unwrap();

        let mut interaction = store.begin();
        interaction.audio_duration_s = Some(2.5);
        interaction.raw_transcription = Some("hello world".into());
        interaction.corrected_transcription = Some("Hello world.".into());
        interaction.route_type = Some("type".into());
        interaction.output_typed = true;
        interaction.output_char_count = Some(12);
        interaction.completed = true;

        store.commit(&interaction);

        // Verify the row exists
        let count: i32 = store
            .conn
            .query_row("SELECT COUNT(*) FROM interactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Cleanup
        let _ = std::fs::remove_file("/tmp/dictate-agent-test-history-commit.db");
    }
}
