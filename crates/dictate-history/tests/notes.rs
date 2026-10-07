//! S35 scratchpad store: round trip, search, privacy, retention, migration.

use dictate_history::{HistoryConfig, HistoryStore, Interaction, NoteError};
use std::path::PathBuf;

fn temp_db(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "dictate-notes-{name}-{}-{}.db",
        std::process::id(),
        uuid_like()
    ))
}

fn uuid_like() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn config(path: &PathBuf) -> HistoryConfig {
    HistoryConfig {
        db_path: path.to_string_lossy().into_owned(),
        ..HistoryConfig::default()
    }
}

fn store(name: &str) -> (HistoryStore, PathBuf) {
    let path = temp_db(name);
    (HistoryStore::new(&config(&path)).unwrap(), path)
}

#[test]
fn a_note_round_trips_newest_first_with_its_word_count() {
    let (store, path) = store("round-trip");
    let first = store.add_note("buy milk and eggs").unwrap();
    let second = store.add_note("  call the dentist  ").unwrap();
    assert!(second.id > first.id);
    assert_eq!(first.word_count, 4);
    assert_eq!(second.text, "call the dentist", "stored text is trimmed");

    let all = store.list_notes(None, None, None).unwrap();
    assert_eq!(
        all.iter().map(|n| n.id).collect::<Vec<_>>(),
        [second.id, first.id]
    );
    assert_eq!(all[1], first);
    assert_eq!(
        store.list_notes(None, None, Some(first.id)).unwrap(),
        vec![first]
    );
    assert!(store.list_notes(None, None, Some(999)).unwrap().is_empty());
    assert_eq!(store.note_count().unwrap(), 2);
    let _ = std::fs::remove_file(path);
}

#[test]
fn notes_survive_reopening_the_database() {
    let (store, path) = store("reopen");
    store.add_note("persisted note").unwrap();
    drop(store);
    let reopened = HistoryStore::new(&config(&path)).unwrap();
    let notes = reopened.list_notes(None, None, None).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "persisted note");
    let _ = std::fs::remove_file(path);
}

#[test]
fn search_is_a_case_insensitive_substring_and_wildcards_are_literal() {
    let (store, path) = store("search");
    store.add_note("Buy Milk").unwrap();
    store.add_note("100% done_today").unwrap();
    store.add_note("unrelated").unwrap();

    let texts = |q: &str| -> Vec<String> {
        store
            .list_notes(Some(q), None, None)
            .unwrap()
            .into_iter()
            .map(|n| n.text)
            .collect()
    };
    assert_eq!(texts("milk"), ["Buy Milk"]);
    assert_eq!(texts("uy mi"), ["Buy Milk"]);
    assert_eq!(texts("100%"), ["100% done_today"]);
    assert_eq!(texts("%"), ["100% done_today"], "% is not a wildcard");
    assert_eq!(texts("_"), ["100% done_today"], "_ is not a wildcard");
    assert_eq!(texts("\\"), Vec::<String>::new());
    assert!(texts("nothing like it").is_empty());
    assert_eq!(texts("   ").len(), 3, "a blank query is no filter");
    let _ = std::fs::remove_file(path);
}

#[test]
fn the_limit_is_honoured_and_bounded() {
    let (store, path) = store("limit");
    for i in 0..5 {
        store.add_note(&format!("note {i}")).unwrap();
    }
    assert_eq!(store.list_notes(None, Some(2), None).unwrap().len(), 2);
    assert_eq!(
        store.list_notes(None, Some(2), None).unwrap()[0].text,
        "note 4"
    );
    assert_eq!(store.list_notes(None, Some(0), None).unwrap().len(), 1);
    assert_eq!(
        store.list_notes(None, Some(u32::MAX), None).unwrap().len(),
        5
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn deleting_removes_the_note_and_its_id_is_never_reused() {
    let (store, path) = store("delete");
    let gone = store.add_note("short lived").unwrap();
    assert!(store.delete_note(gone.id).unwrap());
    assert!(!store.delete_note(gone.id).unwrap(), "already gone");
    assert!(!store.delete_note(12345).unwrap());
    let next = store.add_note("replacement").unwrap();
    assert!(next.id > gone.id, "ids must not be reused");
    assert_eq!(store.list_notes(None, None, None).unwrap().len(), 1);
    let _ = std::fs::remove_file(path);
}

#[test]
fn privacy_mode_stores_nothing_and_says_so() {
    let (mut store, path) = store("privacy");
    store.add_note("stored before privacy").unwrap();
    assert!(store.set_privacy_mode(true));
    match store.add_note("must not be kept") {
        Err(NoteError::Private) => {}
        other => panic!("expected Private, got {other:?}"),
    }
    // Reading what was stored earlier still works.
    assert_eq!(
        store
            .list_notes(None, None, None)
            .unwrap()
            .iter()
            .map(|n| n.text.as_str())
            .collect::<Vec<_>>(),
        ["stored before privacy"]
    );
    let text_in_db: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM notes WHERE text LIKE '%must not%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(text_in_db, 0);
    assert!(!store.set_privacy_mode(false));
    store.add_note("stored after privacy").unwrap();
    assert_eq!(store.note_count().unwrap(), 2);
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_privacy_database_from_config_refuses_notes_too() {
    let path = temp_db("privacy-config");
    let mut c = config(&path);
    c.privacy_mode = true;
    let store = HistoryStore::new(&c).unwrap();
    assert!(matches!(store.add_note("x"), Err(NoteError::Private)));
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_disabled_store_reads_empty_and_refuses_writes() {
    let c = HistoryConfig {
        enabled: false,
        ..HistoryConfig::default()
    };
    let store = HistoryStore::new(&c).unwrap();
    assert!(matches!(store.add_note("x"), Err(NoteError::Disabled)));
    assert!(store.list_notes(None, None, None).unwrap().is_empty());
    assert!(!store.delete_note(1).unwrap());
    assert_eq!(store.note_count().unwrap(), 0);
}

#[test]
fn blank_text_is_not_a_note() {
    let (store, path) = store("blank");
    assert!(matches!(store.add_note("  \n\t "), Err(NoteError::Empty)));
    assert_eq!(store.note_count().unwrap(), 0);
    let _ = std::fs::remove_file(path);
}

#[test]
fn retention_expires_old_notes_with_the_history_window() {
    let path = temp_db("retention");
    let mut c = config(&path);
    c.retention_days = Some(7);
    let store = HistoryStore::new(&c).unwrap();
    let fresh = store.add_note("fresh").unwrap();
    let day_ms = 86_400_000_i64;
    let now = chrono::Utc::now().timestamp_millis();
    for (age_days, text) in [(8, "expired"), (6, "kept")] {
        store
            .connection()
            .execute(
                "INSERT INTO notes (created_at_ms, text, word_count) VALUES (?1, ?2, 1)",
                rusqlite::params![now - age_days * day_ms, text],
            )
            .unwrap();
    }
    assert_eq!(store.apply_retention().unwrap(), 1);
    let mut texts: Vec<String> = store
        .list_notes(None, None, None)
        .unwrap()
        .into_iter()
        .map(|n| n.text)
        .collect();
    texts.sort();
    assert_eq!(texts, ["fresh", "kept"]);
    assert!(store.list_notes(None, None, Some(fresh.id)).unwrap().len() == 1);
    let _ = std::fs::remove_file(path);
}

#[test]
fn without_a_retention_window_notes_are_kept_indefinitely() {
    let (store, path) = store("no-retention");
    store
        .connection()
        .execute(
            "INSERT INTO notes (created_at_ms, text, word_count) VALUES (1, 'ancient', 1)",
            [],
        )
        .unwrap();
    assert_eq!(store.apply_retention().unwrap(), 0);
    assert_eq!(store.note_count().unwrap(), 1);
    let _ = std::fs::remove_file(path);
}

#[test]
fn clearing_history_does_not_touch_notes() {
    let (store, path) = store("purge");
    store.add_note("keep me").unwrap();
    store.commit(&Interaction {
        session_id: "s".into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        corrected_transcription: Some("a dictation".into()),
        completed: true,
        ..Default::default()
    });
    assert_eq!(store.purge().unwrap(), 1);
    assert_eq!(store.note_count().unwrap(), 1);
    let _ = std::fs::remove_file(path);
}

#[test]
fn migration_adds_the_notes_table_and_keeps_every_dictation() {
    let path = temp_db("migration");
    {
        let store = HistoryStore::new(&config(&path)).unwrap();
        store.commit(&Interaction {
            session_id: "legacy".into(),
            timestamp: "2026-01-02T00:00:00+00:00".into(),
            raw_transcription: Some("raw words".into()),
            corrected_transcription: Some("Corrected words.".into()),
            route_type: Some("type".into()),
            completed: true,
            ..Default::default()
        });
        // Turn it back into a schema-version-2 database: no notes table.
        let c = store.connection();
        c.execute_batch(
            "DROP TABLE notes; DELETE FROM schema_version; INSERT INTO schema_version VALUES (2);",
        )
        .unwrap();
    }
    let store = HistoryStore::new(&config(&path)).unwrap();
    let rows: Vec<(String, String)> = store
        .connection()
        .prepare("SELECT session_id, corrected_transcription FROM interactions")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows, [("legacy".into(), "Corrected words.".into())]);
    let version: i64 = store
        .connection()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    assert!(store.add_note("works after migrating").is_ok());
    // Re-opening is a no-op.
    drop(store);
    let again = HistoryStore::new(&config(&path)).unwrap();
    assert_eq!(again.note_count().unwrap(), 1);
    let _ = std::fs::remove_file(path);
}
