//! The scratchpad (S35): quick voice notes kept in the history database.
//!
//! Notes are the one thing dictation can be asked to *keep* rather than type,
//! so they follow the history rules, not the dictionary's:
//!
//! - **Privacy mode refuses the write.** A note that was never stored cannot
//!   leak, and silently dropping it would lose the user's words, so
//!   [`HistoryStore::add_note`] answers [`NoteError::Private`] and the caller
//!   says so out loud.
//! - **Retention applies.** [`HistoryStore::apply_retention`] expires notes
//!   with the same window as dictations.
//! - **Reads work in privacy mode.** Privacy mode stops *new* storage; it does
//!   not hide what was stored before it was switched on.
//!
//! A disabled store has no tables at all, so reads are empty and writes are
//! refused with [`NoteError::Disabled`].

use dictate_proto::Note;
use rusqlite::{params, OptionalExtension};
use std::fmt;

use crate::history::HistoryStore;

/// Notes returned when the caller does not ask for a limit.
const DEFAULT_LIMIT: u32 = 100;
/// Ceiling on notes per request.
const MAX_LIMIT: u32 = 500;

/// Why a note was not stored.
#[derive(Debug)]
pub enum NoteError {
    /// Privacy mode is on (globally, or for this session).
    Private,
    /// History is disabled, so there is nowhere to keep notes.
    Disabled,
    /// There was no text to keep.
    Empty,
    /// SQLite failed.
    Storage(rusqlite::Error),
}

impl fmt::Display for NoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Private => f.write_str("privacy mode is on, so the note was not saved"),
            Self::Disabled => f.write_str("history is disabled, so there is nowhere to keep notes"),
            Self::Empty => f.write_str("there was nothing to note"),
            Self::Storage(e) => write!(f, "could not save the note: {e}"),
        }
    }
}

impl std::error::Error for NoteError {}

impl From<rusqlite::Error> for NoteError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e)
    }
}

impl HistoryStore {
    /// Append a note and return it as stored.
    ///
    /// # Errors
    ///
    /// [`NoteError::Private`] while privacy mode is on, [`NoteError::Disabled`]
    /// with history off, [`NoteError::Empty`] for blank text, otherwise the
    /// SQLite failure.
    pub fn add_note(&self, text: &str) -> Result<Note, NoteError> {
        if !self.enabled {
            return Err(NoteError::Disabled);
        }
        if self.is_privacy_mode() {
            return Err(NoteError::Private);
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(NoteError::Empty);
        }
        let ts_ms = chrono::Utc::now().timestamp_millis();
        let word_count = text.split_whitespace().count() as u32;
        self.conn.execute(
            "INSERT INTO notes (created_at_ms, text, word_count) VALUES (?1, ?2, ?3)",
            params![ts_ms, text, word_count],
        )?;
        let id = self.conn.last_insert_rowid();
        // Keep the window honest after every write, like `commit` does.
        if let Err(e) = self.apply_retention() {
            tracing::error!("Failed to apply history retention: {}", e);
        }
        Ok(Note {
            id,
            ts_ms,
            text: text.to_string(),
            word_count,
        })
    }

    /// Notes, newest first.
    ///
    /// `query` is a case-insensitive substring match (ASCII case folding, SQLite
    /// `LIKE`); `%` and `_` in it are literal. `id` selects one note.
    ///
    /// # Errors
    ///
    /// Propagates SQLite failures.
    pub fn list_notes(
        &self,
        query: Option<&str>,
        limit: Option<u32>,
        id: Option<i64>,
    ) -> rusqlite::Result<Vec<Note>> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let pattern = query
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| format!("%{}%", escape_like(q)));
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at_ms, text, word_count FROM notes
             WHERE (?1 IS NULL OR id = ?1)
               AND (?2 IS NULL OR text LIKE ?2 ESCAPE '\\')
             ORDER BY created_at_ms DESC, id DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![id, pattern, limit], |row| {
            Ok(Note {
                id: row.get(0)?,
                ts_ms: row.get(1)?,
                text: row.get(2)?,
                word_count: row.get::<_, i64>(3)?.try_into().unwrap_or(0),
            })
        })?;
        rows.collect()
    }

    /// Delete one note. Returns whether it existed.
    ///
    /// # Errors
    ///
    /// Propagates SQLite failures.
    pub fn delete_note(&self, id: i64) -> rusqlite::Result<bool> {
        if !self.enabled {
            return Ok(false);
        }
        Ok(self.conn.execute("DELETE FROM notes WHERE id = ?1", [id])? > 0)
    }

    /// How many notes are stored.
    ///
    /// # Errors
    ///
    /// Propagates SQLite failures.
    pub fn note_count(&self) -> rusqlite::Result<u64> {
        if !self.enabled {
            return Ok(0);
        }
        self.conn
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get::<_, i64>(0))
            .optional()
            .map(|n| n.unwrap_or(0).max(0) as u64)
    }
}

fn escape_like(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
