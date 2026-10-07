use anyhow::Result;
use chrono::{Duration, Utc};
use dictate_proto::{DailyWords, HistoryAnalytics};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{error, info};

const SCHEMA_VERSION: i32 = 3;
// S02 deliberately gives the Rust daemon a distinct database from the live
// Python daemon. S30 can optionally import the latter read-only.
const DEFAULT_DB_DIR: &str = "dictated";

pub struct HistoryStore {
    pub(crate) conn: Connection,
    session_id: String,
    pub(crate) enabled: bool,
    privacy_mode: bool,
    pub(crate) retention_days: Option<u32>,
    db_path: Option<PathBuf>,
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

    // History v2 metadata. `None` is deliberately different from zero: the
    // stage may have been skipped or belong to a pre-v2 Python record.
    pub capture_duration_ms: Option<f64>,
    pub vad_duration_ms: Option<f64>,
    pub stt_duration_ms: Option<f64>,
    pub fmt_rules_duration_ms: Option<f64>,
    pub fmt_llm_duration_ms: Option<f64>,
    pub inject_duration_ms: Option<f64>,
    pub app_context: Option<String>,
    pub stt_model: Option<String>,
    pub word_count: Option<u32>,
    /// A per-session no-store request. This is never persisted itself.
    pub no_store: bool,
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
                privacy_mode: true,
                retention_days: None,
                db_path: None,
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

        let mut conn = Connection::open(&db_path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Switching a fresh file to WAL can report SQLITE_BUSY without invoking
        // the busy handler when another opener is doing the same; retry it.
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match conn.execute_batch("PRAGMA journal_mode=WAL") {
                Ok(()) => break,
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::DatabaseBusy && Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => return Err(e.into()),
            }
        }
        conn.execute_batch("PRAGMA secure_delete=ON")?;

        // Install the schema, migrate v1/Python databases in place and stamp
        // the version in ONE transaction (see `install_schema`).
        install_schema(&mut conn, |_| Ok(()))?;

        let session_id = uuid::Uuid::new_v4().to_string()[..12].to_string();
        info!(
            "History store opened at {} (session {})",
            db_path.display(),
            session_id
        );

        let store = Self {
            conn,
            session_id,
            enabled: true,
            privacy_mode: config.privacy_mode,
            retention_days: config.retention_days,
            db_path: Some(db_path),
        };
        if config.import_python_db {
            let _ = store.import_python_db(default_python_db_path());
        }
        store.apply_retention()?;
        Ok(store)
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

    /// Whether global no-store mode is active for this daemon.
    pub fn is_privacy_mode(&self) -> bool {
        self.privacy_mode
    }

    /// Switch global no-store mode on a running daemon.
    ///
    /// Takes effect for the next [`HistoryStore::commit`]. A disabled store
    /// stays in privacy mode whatever is asked: it has nowhere to persist to,
    /// and reporting otherwise would misstate what is recorded. Returns the
    /// mode now in force.
    pub fn set_privacy_mode(&mut self, on: bool) -> bool {
        if self.enabled {
            self.privacy_mode = on;
        }
        self.privacy_mode
    }

    /// Run a [`dictate_proto::HistoryQuery`] against the log.
    ///
    /// # Errors
    ///
    /// Propagates SQLite failures.
    pub fn query(&self, q: &dictate_proto::HistoryQuery) -> Result<dictate_proto::HistoryPage> {
        crate::query::query(&self.conn, q)
    }

    /// Remove every persisted interaction **and note** and return the number of
    /// rows removed (dictations plus notes).
    pub fn purge(&self) -> Result<u64> {
        if !self.enabled {
            return Ok(0);
        }
        let removed = self.conn.execute("DELETE FROM interactions", [])? as u64;
        // An explicit privacy purge clears the scratchpad too: notes are
        // history-class data in the same file, and a purge that left them
        // behind would not do what it says.
        let notes = self.conn.execute("DELETE FROM notes", [])? as u64;
        // secure_delete overwrites deleted cells in the main database. A WAL
        // checkpoint plus VACUUM removes residual pages from both files so an
        // explicit privacy purge is stronger than ordinary retention cleanup.
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM")?;
        Ok(removed + notes)
    }

    /// Enforce the configured retention window now and after each commit.
    pub fn apply_retention(&self) -> Result<u64> {
        if !self.enabled {
            return Ok(0);
        }
        let Some(days) = self.retention_days else {
            return Ok(0);
        };
        let cutoff_time = Utc::now() - Duration::days(i64::from(days));
        let removed = self.conn.execute(
            "DELETE FROM interactions WHERE timestamp < ?1",
            [cutoff_time.to_rfc3339()],
        )? as u64;
        // Notes share the history window (S35).
        let notes = self.conn.execute(
            "DELETE FROM notes WHERE created_at_ms < ?1",
            [cutoff_time.timestamp_millis()],
        )? as u64;
        Ok(removed + notes)
    }

    /// Return WPM, daily word totals, and active-day streaks.
    pub fn analytics(&self) -> Result<HistoryAnalytics> {
        if !self.enabled {
            return Ok(empty_analytics());
        }
        let mut stmt = self.conn.prepare(
            "SELECT substr(timestamp, 1, 10), word_count, corrected_transcription, audio_duration_s
             FROM interactions WHERE completed = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<u32>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<f64>>(3)?,
            ))
        })?;
        let mut words_by_day = BTreeMap::<String, u64>::new();
        let mut words_with_audio = 0_u64;
        let mut audio_s = 0.0_f64;
        for row in rows {
            let (day, stored_words, text, duration) = row?;
            let count = stored_words
                .map(u64::from)
                .unwrap_or_else(|| text.as_deref().map_or(0, word_count));
            *words_by_day.entry(day).or_default() += count;
            let duration = duration.unwrap_or(0.0).max(0.0);
            if count > 0 && duration > 0.0 {
                words_with_audio += count;
                audio_s += duration;
            }
        }
        let today = Utc::now().date_naive();
        let today_key = today.format("%F").to_string();
        Ok(HistoryAnalytics {
            overall_wpm: (audio_s > 0.0).then_some(words_with_audio as f64 / (audio_s / 60.0)),
            words_today: words_by_day.get(&today_key).copied().unwrap_or(0),
            words_by_day: words_by_day
                .iter()
                .map(|(day, words)| DailyWords {
                    day: day.clone(),
                    words: *words,
                })
                .collect(),
            current_streak_days: streak_ending_at(&words_by_day, today),
            longest_streak_days: longest_streak(&words_by_day),
        })
    }

    /// Copy a legacy Python database once per source path without modifying it.
    pub fn import_python_db(&self, path: impl AsRef<Path>) -> Result<u64> {
        if !self.enabled {
            return Ok(0);
        }
        let path = path.as_ref();
        if !path.exists() || self.db_path.as_deref().is_some_and(|db| db == path) {
            return Ok(0);
        }
        let source = path.to_string_lossy().into_owned();
        let imported_before: Option<i64> = self
            .conn
            .query_row(
                "SELECT row_count FROM history_imports WHERE source_path = ?1",
                [source.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if imported_before.is_some() {
            return Ok(0);
        }
        let legacy = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        if !table_exists(&legacy, "interactions")? {
            return Ok(0);
        }
        drop(legacy);
        self.conn.execute(
            "ATTACH DATABASE ?1 AS python_history",
            [path.to_string_lossy().as_ref()],
        )?;
        let import_result = (|| -> Result<u64> {
            // Copied rows and the idempotency marker commit together. If the
            // marker fails, dropping the transaction rolls back the rows so a
            // retry cannot duplicate legacy history.
            let transaction = self.conn.unchecked_transaction()?;
            let imported = transaction.execute(
                "INSERT INTO interactions (
                session_id, timestamp, audio_duration_s, raw_transcription,
                corrected_transcription, transcription_duration_s, grammar_input,
                grammar_output, grammar_changed, grammar_error, grammar_duration_s,
                route_type, route_model, route_trigger, route_confidence, prompt_sent,
                response_text, execution_model, execution_duration_s, execution_success,
                execution_error, output_typed, output_char_count, total_duration_s,
                completed, error_summary
             ) SELECT
                session_id, timestamp, audio_duration_s, raw_transcription,
                corrected_transcription, transcription_duration_s, grammar_input,
                grammar_output, grammar_changed, grammar_error, grammar_duration_s,
                route_type, route_model, route_trigger, route_confidence, prompt_sent,
                response_text, execution_model, execution_duration_s, execution_success,
                execution_error, output_typed, output_char_count, total_duration_s,
                completed, error_summary
             FROM python_history.interactions",
                [],
            )? as u64;
            transaction.execute(
                "INSERT INTO history_imports (source_path, imported_at, row_count) VALUES (?1, ?2, ?3)",
                params![source, Utc::now().to_rfc3339(), imported as i64],
            )?;
            transaction.commit()?;
            Ok(imported)
        })();
        let detach_result = self.conn.execute("DETACH DATABASE python_history", []);
        if let Err(detach_error) = detach_result {
            if import_result.is_ok() {
                return Err(detach_error.into());
            }
            error!(
                "legacy import failed and attached database cleanup also failed: {detach_error}"
            );
        }
        import_result
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
        if !self.enabled || self.privacy_mode || interaction.no_store {
            return;
        }
        let total_duration = interaction
            .start_time
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        if let Err(e) = self.insert(interaction, total_duration) {
            error!("Failed to commit interaction: {}", e);
        } else if let Err(e) = self.apply_retention() {
            error!("Failed to apply history retention: {}", e);
        }
    }

    fn insert(&self, i: &Interaction, total_duration: f64) -> rusqlite::Result<()> {
        // A scratchpad note's body lives in `notes` and nowhere else, so that
        // deleting the note removes its only copy. Its interaction row keeps
        // the route, timings and word count but no text, and so nothing for
        // `interactions_fts` to index.
        let is_note = i.route_type.as_deref() == Some("note");
        let text = |value: &Option<String>| -> Option<String> {
            if is_note {
                None
            } else {
                value.clone()
            }
        };
        self.conn.execute(
            "INSERT INTO interactions (
                session_id, timestamp, audio_duration_s,
                raw_transcription, corrected_transcription, transcription_duration_s,
                grammar_input, grammar_output, grammar_changed, grammar_error, grammar_duration_s,
                route_type, route_model, route_trigger, route_confidence,
                prompt_sent, response_text, execution_model, execution_duration_s,
                execution_success, execution_error,
                output_typed, output_char_count,
                total_duration_s, completed, error_summary,
                capture_duration_ms, vad_duration_ms, stt_duration_ms,
                fmt_rules_duration_ms, fmt_llm_duration_ms, inject_duration_ms,
                app_context, stt_model, word_count
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21,
                ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32,
                ?33, ?34, ?35
            )",
            params![
                i.session_id,
                i.timestamp,
                i.audio_duration_s,
                text(&i.raw_transcription),
                text(&i.corrected_transcription),
                i.transcription_duration_s,
                text(&i.grammar_input),
                text(&i.grammar_output),
                i.grammar_changed as i32,
                i.grammar_error,
                i.grammar_duration_s,
                i.route_type,
                i.route_model,
                i.route_trigger,
                i.route_confidence,
                text(&i.prompt_sent),
                text(&i.response_text),
                i.execution_model,
                i.execution_duration_s,
                i.execution_success.map(|b| b as i32),
                i.execution_error,
                i.output_typed as i32,
                i.output_char_count.map(|c| c as i64),
                total_duration,
                i.completed as i32,
                i.error_summary,
                i.capture_duration_ms,
                i.vad_duration_ms,
                i.stt_duration_ms,
                i.fmt_rules_duration_ms,
                i.fmt_llm_duration_ms,
                i.inject_duration_ms,
                i.app_context,
                i.stt_model,
                i.word_count.map(i64::from),
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

fn default_python_db_path() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".local/share/dictate-agent/history.db")
}

/// The version stamped in `schema_version`, or 0 for a database that has no
/// such table (brand new) or an empty one (an interrupted pre-atomic upgrade).
fn stored_version(conn: &Connection) -> Result<i64> {
    if !table_exists(conn, "schema_version")? {
        return Ok(0);
    }
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |r| r.get(0),
    )?)
}

/// Install the schema, migrate old shapes and stamp the version atomically.
///
/// Everything runs inside a single `BEGIN IMMEDIATE` transaction:
///
/// * the write lock is taken *before* the stored version is read, so a
///   concurrent opener waits and then sees the finished migration instead of
///   running `ALTER TABLE` a second time;
/// * a database stamped newer than this binary is refused before any schema
///   statement runs, and is left exactly as found;
/// * a failure or crash at any point rolls back to the old version with
///   nothing half-applied (SQLite DDL is transactional);
/// * an already-current database skips the schema, the column checks and the
///   FTS rebuild entirely.
///
/// `before_stamp` runs after the schema and migration but before the version
/// is written; tests use it to inject a failure at the worst moment.
fn install_schema(
    conn: &mut Connection,
    before_stamp: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<()>,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let stored = stored_version(&tx)?;
    anyhow::ensure!(
        stored <= i64::from(SCHEMA_VERSION),
        "history database is schema version {stored}, newer than this dictated \
         (supports up to {SCHEMA_VERSION}); refusing to open or modify it. \
         Upgrade dictated or point [history] db_path at another file"
    );
    if stored == i64::from(SCHEMA_VERSION) {
        return Ok(());
    }
    tx.execute_batch(include_str!("../../../sql/schema.sql"))?;
    migrate(&tx)?;
    before_stamp(&tx)?;
    tx.execute("DELETE FROM schema_version", [])?;
    tx.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        [SCHEMA_VERSION],
    )?;
    tx.commit()?;
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    // SQLite's CREATE IF NOT EXISTS never adds columns to an existing table.
    for (name, ty) in [
        ("capture_duration_ms", "REAL"),
        ("vad_duration_ms", "REAL"),
        ("stt_duration_ms", "REAL"),
        ("fmt_rules_duration_ms", "REAL"),
        ("fmt_llm_duration_ms", "REAL"),
        ("inject_duration_ms", "REAL"),
        ("app_context", "TEXT"),
        ("stt_model", "TEXT"),
        ("word_count", "INTEGER"),
    ] {
        if !column_exists(conn, "interactions", name)? {
            conn.execute_batch(&format!("ALTER TABLE interactions ADD COLUMN {name} {ty}"))?;
        }
    }
    // Backfill the external-content FTS table after creating it over old rows.
    conn.execute(
        "INSERT INTO interactions_fts(interactions_fts) VALUES ('rebuild')",
        [],
    )?;
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.iter().any(|name| name == column))
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn word_count(text: &str) -> u64 {
    text.split_whitespace().count() as u64
}

fn empty_analytics() -> HistoryAnalytics {
    HistoryAnalytics {
        overall_wpm: None,
        words_today: 0,
        words_by_day: Vec::new(),
        current_streak_days: 0,
        longest_streak_days: 0,
    }
}

fn streak_ending_at(days: &BTreeMap<String, u64>, mut day: chrono::NaiveDate) -> u32 {
    let mut streak = 0;
    loop {
        if days
            .get(&day.format("%F").to_string())
            .copied()
            .unwrap_or(0)
            == 0
        {
            return streak;
        }
        streak += 1;
        day -= Duration::days(1);
    }
}

fn longest_streak(days: &BTreeMap<String, u64>) -> u32 {
    let mut longest = 0;
    let mut running = 0;
    let mut previous = None;
    for (day, words) in days {
        let Ok(day) = chrono::NaiveDate::parse_from_str(day, "%F") else {
            continue;
        };
        if *words > 0 && previous.is_some_and(|p: chrono::NaiveDate| p + Duration::days(1) == day) {
            running += 1;
        } else if *words > 0 {
            running = 1;
        } else {
            running = 0;
        }
        longest = longest.max(running);
        previous = Some(day);
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(path: PathBuf) -> crate::config::HistoryConfig {
        crate::config::HistoryConfig {
            enabled: true,
            db_path: path.to_string_lossy().into_owned(),
            max_response_length: 10_000,
            ..Default::default()
        }
    }

    fn temp_db(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dictate-history-{name}-{}-{}.db",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    fn create_v1_database(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE interactions (
                id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, timestamp TEXT NOT NULL,
                audio_duration_s REAL, raw_transcription TEXT, corrected_transcription TEXT,
                transcription_duration_s REAL, grammar_input TEXT, grammar_output TEXT,
                grammar_changed INTEGER, grammar_error TEXT, grammar_duration_s REAL,
                route_type TEXT, route_model TEXT, route_trigger TEXT, route_confidence REAL,
                prompt_sent TEXT, response_text TEXT, execution_model TEXT, execution_duration_s REAL,
                execution_success INTEGER, execution_error TEXT, output_typed INTEGER,
                output_char_count INTEGER, total_duration_s REAL, completed INTEGER DEFAULT 0,
                error_summary TEXT
            );
            CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
            INSERT INTO schema_version VALUES (1);",
        )
        .unwrap();
    }

    #[test]
    fn test_history_store_create() {
        let config = crate::config::HistoryConfig {
            enabled: true,
            db_path: "/tmp/dictate-agent-test-history.db".into(),
            max_response_length: 10000,
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

    fn tables(conn: &Connection) -> Vec<String> {
        conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn version_rows(conn: &Connection) -> Vec<i64> {
        conn.prepare("SELECT version FROM schema_version")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// A schema-version-2 database: a real dictation row, no `notes` table.
    fn create_v2_database(path: &Path) {
        {
            let store = HistoryStore::new(&config(path.to_path_buf())).unwrap();
            store.commit(&Interaction {
                session_id: "legacy".into(),
                timestamp: "2026-01-02T00:00:00+00:00".into(),
                raw_transcription: Some("raw".into()),
                corrected_transcription: Some("Corrected.".into()),
                completed: true,
                ..Default::default()
            });
            store
                .connection()
                .execute_batch(
                    "DROP TABLE notes;
                     DELETE FROM schema_version; INSERT INTO schema_version VALUES (2);",
                )
                .unwrap();
        }
    }

    #[test]
    fn failed_upgrade_leaves_a_v2_database_at_v2_with_nothing_half_applied() {
        let path = temp_db("atomic-v2");
        create_v2_database(&path);
        let mut conn = Connection::open(&path).unwrap();
        // Fail after the schema and migration ran, right before the stamp.
        let err = install_schema(&mut conn, |tx| {
            assert!(table_exists(tx, "notes").unwrap(), "notes created in-tx");
            anyhow::bail!("injected stamp failure")
        })
        .unwrap_err();
        assert!(err.to_string().contains("injected"));
        assert!(!table_exists(&conn, "notes").unwrap());
        assert_eq!(version_rows(&conn), [2]);
        drop(conn);
        // A retry recovers and keeps the dictation.
        let store = HistoryStore::new(&config(path.clone())).unwrap();
        assert_eq!(version_rows(store.connection()), [3]);
        let n: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM interactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_crash_mid_upgrade_rolls_back_to_the_old_version() {
        let path = temp_db("atomic-crash");
        create_v1_database(&path);
        let mut conn = Connection::open(&path).unwrap();
        // A panic unwinds with the transaction open and the connection dropped
        // without commit, which is what a killed process looks like to SQLite.
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = install_schema(&mut conn, |_| panic!("simulated crash"));
        }));
        assert!(crashed.is_err());
        drop(conn);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(version_rows(&conn), [1]);
        assert!(!column_exists(&conn, "interactions", "word_count").unwrap());
        assert!(!table_exists(&conn, "notes").unwrap());
        assert!(!table_exists(&conn, "interactions_fts").unwrap());
        drop(conn);
        // And the next open completes the upgrade.
        let store = HistoryStore::new(&config(path)).unwrap();
        assert_eq!(version_rows(store.connection()), [SCHEMA_VERSION as i64]);
        assert!(column_exists(store.connection(), "interactions", "word_count").unwrap());
    }

    #[test]
    fn failed_first_open_leaves_an_empty_database() {
        let path = temp_db("atomic-fresh");
        let mut conn = Connection::open(&path).unwrap();
        let _ = install_schema(&mut conn, |_| anyhow::bail!("injected")).unwrap_err();
        assert!(tables(&conn).is_empty(), "{:?}", tables(&conn));
    }

    #[test]
    fn a_newer_database_is_refused_and_left_untouched() {
        let path = temp_db("newer");
        {
            // Only a version table, stamped from the future: if schema.sql
            // ever ran against it, `interactions` would appear.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
                 INSERT INTO schema_version VALUES (4);
                 CREATE TABLE future_only (x TEXT); INSERT INTO future_only VALUES ('keep');",
            )
            .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let err = HistoryStore::new(&config(path.clone()))
            .err()
            .expect("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("newer") && msg.contains('4'), "{msg}");
        let conn = Connection::open(&path).unwrap();
        assert_eq!(version_rows(&conn), [4]);
        assert_eq!(tables(&conn), ["future_only", "schema_version"]);
        drop(conn);
        // WAL mode may add a -wal file but the main file's pages are unchanged
        // apart from the journal-mode header bytes; the data must be intact.
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before.len(), after.len());
    }

    #[test]
    fn an_already_current_database_skips_the_schema_and_fts_rebuild() {
        let path = temp_db("current");
        drop(HistoryStore::new(&config(path.clone())).unwrap());
        // Dropping a trigger proves schema.sql is not re-run on a current DB.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER interactions_ai").unwrap();
        drop(conn);
        let store = HistoryStore::new(&config(path)).unwrap();
        let n: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'interactions_ai'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn v1_database_is_migrated_in_place_and_rebuilt_for_fts() {
        let path = temp_db("migration");
        create_v1_database(&path);
        let legacy = Connection::open(&path).unwrap();
        legacy.execute(
            "INSERT INTO interactions (session_id, timestamp, corrected_transcription, raw_transcription, completed)
             VALUES ('old', '2026-01-02T00:00:00+00:00', 'history survives migration', 'history survives migration', 1)",
            [],
        ).unwrap();
        drop(legacy);

        let store = HistoryStore::new(&config(path.clone())).unwrap();
        let version: i32 = store
            .connection()
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        for column in [
            "capture_duration_ms",
            "app_context",
            "stt_model",
            "word_count",
        ] {
            assert!(column_exists(store.connection(), "interactions", column).unwrap());
        }
        let page = store
            .query(&dictate_proto::HistoryQuery {
                text: Some("survives migration".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.total, Some(1));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn analytics_reports_weighted_wpm_daily_words_and_streaks() {
        let path = temp_db("analytics");
        let store = HistoryStore::new(&config(path.clone())).unwrap();
        let today = Utc::now().date_naive();
        for (day, text) in [
            (today - Duration::days(1), "one two"),
            (today, "one two three four"),
        ] {
            store.commit(&Interaction {
                session_id: "test".into(),
                timestamp: format!("{day}T12:00:00+00:00"),
                corrected_transcription: Some(text.into()),
                audio_duration_s: Some(2.0),
                word_count: Some(text.split_whitespace().count() as u32),
                completed: true,
                ..Default::default()
            });
        }
        let analytics = store.analytics().unwrap();
        assert_eq!(analytics.words_today, 4);
        assert_eq!(analytics.current_streak_days, 2);
        assert_eq!(analytics.longest_streak_days, 2);
        assert_eq!(analytics.words_by_day.len(), 2);
        assert!((analytics.overall_wpm.unwrap() - 90.0).abs() < f64::EPSILON);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn privacy_mode_switches_live_and_a_disabled_store_stays_private() {
        let path = temp_db("live-privacy");
        let mut store = HistoryStore::new(&config(path.clone())).unwrap();
        let rows = |store: &HistoryStore| -> i64 {
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
                .unwrap()
        };
        assert!(store.set_privacy_mode(true));
        store.commit(&Interaction {
            corrected_transcription: Some("synthetic private text".into()),
            completed: true,
            ..store.begin()
        });
        assert_eq!(rows(&store), 0, "privacy mode must stop the next commit");
        assert!(!store.set_privacy_mode(false));
        store.commit(&Interaction {
            corrected_transcription: Some("synthetic public text".into()),
            completed: true,
            ..store.begin()
        });
        assert_eq!(rows(&store), 1);

        let mut settings = config(path);
        settings.enabled = false;
        let mut disabled = HistoryStore::new(&settings).unwrap();
        assert!(
            disabled.set_privacy_mode(false),
            "a store with nowhere to persist must not claim to be recording"
        );
    }

    #[test]
    fn privacy_retention_purge_and_python_import_are_safe() {
        let path = temp_db("privacy");
        let mut settings = config(path.clone());
        settings.privacy_mode = true;
        let private = HistoryStore::new(&settings).unwrap();
        private.commit(&Interaction {
            corrected_transcription: Some("do not retain this".into()),
            ..private.begin()
        });
        let count: i64 = private
            .connection()
            .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        drop(private);

        let mut settings = config(path.clone());
        settings.retention_days = Some(7);
        let store = HistoryStore::new(&settings).unwrap();
        let secure_delete: i64 = store
            .connection()
            .query_row("PRAGMA secure_delete", [], |row| row.get(0))
            .unwrap();
        assert_eq!(secure_delete, 1);
        for (timestamp, text) in [
            ((Utc::now() - Duration::days(8)).to_rfc3339(), "expired"),
            ((Utc::now() - Duration::days(6)).to_rfc3339(), "kept"),
        ] {
            store.commit(&Interaction {
                session_id: "retention".into(),
                timestamp,
                corrected_transcription: Some(text.into()),
                completed: true,
                ..Default::default()
            });
        }
        assert_eq!(store.purge().unwrap(), 1);

        let source = temp_db("python-source");
        create_v1_database(&source);
        Connection::open(&source).unwrap().execute(
            "INSERT INTO interactions (session_id, timestamp, corrected_transcription, route_type, completed)
             VALUES ('python', '2026-01-02T00:00:00+00:00', 'imported from python', 'type', 1)",
            [],
        ).unwrap();
        store
            .connection()
            .execute_batch(
                "CREATE TRIGGER reject_import_marker BEFORE INSERT ON history_imports
             BEGIN SELECT RAISE(ABORT, 'marker rejected'); END;",
            )
            .unwrap();
        assert!(store.import_python_db(&source).is_err());
        let rolled_back: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rolled_back, 0, "failed import must not leave copied rows");
        store
            .connection()
            .execute_batch("DROP TRIGGER reject_import_marker")
            .unwrap();
        assert_eq!(store.import_python_db(&source).unwrap(), 1);
        assert_eq!(store.import_python_db(&source).unwrap(), 0);
        let source_count: i64 = Connection::open(&source)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(source_count, 1, "legacy source must remain unchanged");
        let _ = std::fs::remove_file(source);
        let _ = std::fs::remove_file(path);
    }
}
