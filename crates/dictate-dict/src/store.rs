use chrono::Utc;
use dictate_proto::{DictionaryEntry, EntrySource, ErrorCode, ProtoError};
use rusqlite::{params, Connection, OptionalExtension};
use std::{collections::HashMap, path::Path, time::Duration};

/// S24 appends migration 3 here for snippets. Never change an applied migration.
pub const SCHEMA_VERSION: u32 = 2;
pub const MIGRATION_V1: &str = "CREATE TABLE entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    phrase TEXT NOT NULL, phrase_key TEXT NOT NULL UNIQUE,
    sounds_like TEXT NOT NULL, case_sensitive INTEGER NOT NULL,
    enabled INTEGER NOT NULL, source TEXT NOT NULL, hit_count INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);";
const MIGRATIONS: &[&str] = &[
    MIGRATION_V1,
    "ALTER TABLE entries ADD COLUMN apps TEXT NOT NULL DEFAULT '[]';",
];

#[derive(Debug, Clone)]
pub struct StoredEntry {
    pub entry: DictionaryEntry,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct DictionaryStore {
    conn: Connection,
}
impl DictionaryStore {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        Self::from_connection(Connection::open(path)?)
    }
    pub fn in_memory() -> anyhow::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }
    pub fn from_connection(mut conn: Connection) -> anyhow::Result<Self> {
        conn.busy_timeout(Duration::from_secs(2))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(
            version <= SCHEMA_VERSION,
            "dictionary database is newer than this daemon"
        );
        for v in version as usize..MIGRATIONS.len() {
            let tx = conn.transaction()?;
            tx.execute_batch(MIGRATIONS[v])?;
            tx.pragma_update(None, "user_version", v + 1)?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }
    pub fn entries(&self) -> anyhow::Result<Vec<StoredEntry>> {
        let mut stmt = self.conn.prepare("SELECT id, phrase, sounds_like, case_sensitive, enabled, source, hit_count, apps, created_at, updated_at FROM entries ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, u64>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                phrase,
                aliases,
                case_sensitive,
                enabled,
                source,
                hits,
                apps,
                created_at,
                updated_at,
            ) = row?;
            Ok(StoredEntry {
                entry: DictionaryEntry {
                    id: Some(id),
                    phrase,
                    sounds_like: serde_json::from_str(&aliases)?,
                    apps: serde_json::from_str(&apps)?,
                    case_sensitive,
                    enabled,
                    source: EntrySource::from(source.as_str()),
                    hit_count: Some(hits),
                },
                created_at,
                updated_at,
            })
        })
        .collect()
    }
    pub fn upsert(&mut self, mut entry: DictionaryEntry) -> Result<DictionaryEntry, ProtoError> {
        validate(&entry)?;
        let now = Utc::now().timestamp_millis();
        let aliases = serde_json::to_string(&entry.sounds_like).map_err(internal)?;
        let apps = serde_json::to_string(&entry.apps).map_err(internal)?;
        let tx = self.conn.transaction().map_err(internal)?;
        let key = entry.phrase.to_lowercase();
        let conflicting: Option<i64> = tx
            .query_row("SELECT id FROM entries WHERE phrase_key=?1", [&key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(internal)?;
        if conflicting.is_some() && conflicting != entry.id {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "dictionary phrase already exists",
            ));
        }
        if let Some(id) = entry.id {
            let changed = tx.execute("UPDATE entries SET phrase=?1,phrase_key=?2,sounds_like=?3,case_sensitive=?4,enabled=?5,source=?6,apps=?7,updated_at=?8 WHERE id=?9",
                params![entry.phrase,key,aliases,entry.case_sensitive,entry.enabled,entry.source.as_str(),apps,now,id]).map_err(internal)?;
            if changed == 0 {
                return Err(ProtoError::new(
                    ErrorCode::NotFound,
                    "dictionary entry does not exist",
                ));
            }
        } else {
            tx.execute("INSERT INTO entries (phrase,phrase_key,sounds_like,case_sensitive,enabled,source,apps,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
                params![entry.phrase,key,aliases,entry.case_sensitive,entry.enabled,entry.source.as_str(),apps,now]).map_err(internal)?;
            entry.id = Some(tx.last_insert_rowid());
        }
        // Hit counts are server-owned, including on import and update.
        entry.hit_count = Some(
            tx.query_row(
                "SELECT hit_count FROM entries WHERE id=?1",
                [entry.id],
                |r| r.get(0),
            )
            .map_err(internal)?,
        );
        tx.commit().map_err(internal)?;
        Ok(entry)
    }
    pub fn delete(&mut self, id: i64) -> Result<(), ProtoError> {
        if id <= 0 {
            return Err(invalid("dictionary id must be positive"));
        }
        if self
            .conn
            .execute("DELETE FROM entries WHERE id=?1", [id])
            .map_err(internal)?
            == 0
        {
            return Err(ProtoError::new(
                ErrorCode::NotFound,
                "dictionary entry does not exist",
            ));
        }
        Ok(())
    }
    pub fn increment_hits(&mut self, hits: &HashMap<i64, u64>) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        for (id, count) in hits {
            tx.execute(
                "UPDATE entries SET hit_count=hit_count+?1 WHERE id=?2",
                params![count, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
fn valid_text(s: &str) -> bool {
    !s.trim().is_empty()
        && s == s.trim()
        && s.chars().count() <= 200
        && !s.chars().any(char::is_control)
}
pub fn validate(e: &DictionaryEntry) -> Result<(), ProtoError> {
    if !valid_text(&e.phrase) {
        return Err(invalid(
            "phrase must contain 1..200 characters, no control characters or outer whitespace",
        ));
    }
    if e.id.is_some_and(|id| id <= 0) {
        return Err(invalid("dictionary id must be positive"));
    }
    if e.sounds_like.len() > 64
        || e.apps.len() > 64
        || e.sounds_like.iter().chain(&e.apps).any(|s| !valid_text(s))
    {
        return Err(invalid(
            "aliases and apps must contain at most 64 nonempty strings of 1..200 characters",
        ));
    }
    if !e.source.is_known() {
        return Err(invalid("unknown dictionary entry source"));
    }
    Ok(())
}
fn invalid(s: &str) -> ProtoError {
    ProtoError::new(ErrorCode::InvalidParams, s)
}
pub(crate) fn internal(e: impl std::fmt::Display) -> ProtoError {
    ProtoError::new(
        ErrorCode::Internal,
        format!("dictionary operation failed: {e}"),
    )
}
