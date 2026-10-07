//! #1490: two simultaneous opens must serialize on the migration and never
//! both run `ALTER TABLE` / both stamp the version.

use dictate_history::{HistoryConfig, HistoryStore};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

const ROUNDS: usize = 20;
const OPENERS: usize = 6;

fn temp_db(name: &str, round: usize) -> PathBuf {
    std::env::temp_dir().join(format!(
        "dictate-concurrent-{name}-{round}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn config(path: &Path) -> HistoryConfig {
    HistoryConfig {
        db_path: path.to_string_lossy().into_owned(),
        ..HistoryConfig::default()
    }
}

/// A Python-era / schema-version-1 database holding synthetic rows.
fn v1(path: &Path) {
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
    for i in 0..200 {
        conn.execute(
            "INSERT INTO interactions (session_id, timestamp, corrected_transcription, raw_transcription, completed)
             VALUES ('s', '2026-01-02T00:00:00+00:00', ?1, ?1, 1)",
            [format!("synthetic sentence number {i}")],
        )
        .unwrap();
    }
}

/// A schema-version-2 database: dictations, no `notes` table.
fn v2(path: &Path) {
    v1(path);
    // Bring it to v2's shape by opening once, then dropping the v3 table.
    let store = HistoryStore::new(&config(path)).unwrap();
    store
        .connection()
        .execute_batch(
            "DROP TABLE notes; DELETE FROM schema_version; INSERT INTO schema_version VALUES (2);",
        )
        .unwrap();
}

fn race(shape: &str, prepare: fn(&Path)) {
    for round in 0..ROUNDS {
        let path = temp_db(shape, round);
        prepare(&path);
        let rows_before = if path.exists() { count(&path) } else { 0 };
        let barrier = Arc::new(Barrier::new(OPENERS));
        let handles: Vec<_> = (0..OPENERS)
            .map(|_| {
                let (barrier, path) = (barrier.clone(), path.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    HistoryStore::new(&config(&path))
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
            })
            .collect();
        for h in handles {
            h.join()
                .unwrap()
                .unwrap_or_else(|e| panic!("{shape} round {round}: concurrent open failed: {e}"));
        }
        let conn = Connection::open(&path).unwrap();
        let versions: Vec<i64> = conn
            .prepare("SELECT version FROM schema_version")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(versions, [3], "{shape} round {round}");
        drop(conn);
        assert_eq!(
            count(&path),
            rows_before,
            "{shape} round {round}: rows survive"
        );
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }
}

fn count(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM interactions", [], |r| r.get(0))
        .unwrap_or(0)
}

#[test]
fn simultaneous_opens_of_a_fresh_database() {
    race("fresh", |_| {});
}

#[test]
fn simultaneous_opens_of_a_v1_database_with_rows() {
    race("v1", v1);
}

#[test]
fn simultaneous_opens_of_a_v2_database_with_rows() {
    race("v2", v2);
}
