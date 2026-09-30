use dictate_dict::suggestions::{mine, mine_path};
use dictate_proto::DictionaryEntry;
use rusqlite::{params, Connection};
fn history() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE interactions (timestamp TEXT, grammar_input TEXT, grammar_output TEXT, corrected_transcription TEXT, grammar_error TEXT, completed INTEGER)").unwrap();
    c
}
fn add(c: &Connection, day: u32, a: &str, b: &str) {
    c.execute(
        "INSERT INTO interactions VALUES (?1,?2,?3,?3,NULL,1)",
        params![format!("2026-09-{day:02}T12:00:00+00:00"), a, b],
    )
    .unwrap();
}
#[test]
fn recurring_terms_rewrites_thresholds_dates_and_known_entries() {
    let c = history();
    for day in [1, 1, 2] {
        add(
            &c,
            day,
            "deploy kube net ease today",
            "deploy Kubernetes today",
        );
    }
    for day in 1..=5 {
        add(
            &c,
            day,
            "We use Tauri and HTTP with WidgetKit",
            "We use Tauri and HTTP with WidgetKit",
        );
    }
    for day in 1..=4 {
        add(
            &c,
            day,
            "Ordinary sentence first Capital",
            "Ordinary sentence first Capital",
        );
    }
    let s = mine(&c, &[]).unwrap();
    let rewrite = s.iter().find(|s| s.entry.phrase == "Kubernetes").unwrap();
    assert_eq!(rewrite.entry.sounds_like, vec!["kube net ease"]);
    assert_eq!((rewrite.count, rewrite.days), (3, 2));
    assert!(rewrite.first_seen.starts_with("2026-09-01"));
    assert!(rewrite.last_seen.starts_with("2026-09-02"));
    for term in ["Tauri", "HTTP", "WidgetKit"] {
        assert!(s.iter().any(|s| s.entry.phrase == term && s.count == 5));
    }
    for term in ["We", "Ordinary", "Capital"] {
        assert!(!s.iter().any(|s| s.entry.phrase == term));
    }
    assert!(!mine(&c, &[DictionaryEntry::new("Tauri")])
        .unwrap()
        .iter()
        .any(|s| s.entry.phrase == "Tauri"));
    assert_eq!(
        c.query_row("SELECT count(*) FROM interactions", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        12
    );
}
#[test]
fn same_day_inconsistent_insertions_punctuation_and_errors_are_not_rewrites() {
    let c = history();
    for _ in 0..5 {
        add(&c, 1, "use kube net ease", "use Kubernetes");
    }
    for day in 1..=3 {
        add(&c, day, "deploy widget", "deploy WidgetOne");
        add(&c, day, "deploy widget", "deploy WidgetTwo");
        add(&c, day, "do this", "do this please");
        add(&c, day, "hello friend", "hello, friend!");
    }
    c.execute("INSERT INTO interactions VALUES ('2026-09-02T12:00:00+00:00','use kube net ease','use Kubernetes',NULL,'failed',1)",[]).unwrap();
    assert!(mine(&c, &[])
        .unwrap()
        .iter()
        .all(|s| s.reason != "consistent_rewrite"));
}
#[test]
fn missing_disabled_and_readonly_history() {
    let c = Connection::open_in_memory().unwrap();
    assert!(mine(&c, &[]).unwrap().is_empty());
    assert!(mine_path("/tmp/dictate-s22-nonexistent-history.db", &[]).is_err());
    let path = std::env::temp_dir().join(format!("s22-history-{}.db", std::process::id()));
    let c = Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE interactions (timestamp TEXT, grammar_input TEXT, grammar_output TEXT, corrected_transcription TEXT, grammar_error TEXT, completed INTEGER)").unwrap();
    for day in 1..=5 {
        add(&c, day, "Use Tauri now", "Use Tauri now");
    }
    drop(c);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(mine_path(&path, &[]).unwrap().len(), 1);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn quoted_sentence_initial_words_are_not_recurring_terms() {
    let c = history();
    for day in 1..=5 {
        add(
            &c,
            day,
            "\"Hello friend.\" 'Welcome back!'",
            "\"Hello friend.\" 'Welcome back!'",
        );
    }
    assert!(mine(&c, &[]).unwrap().is_empty());
}
