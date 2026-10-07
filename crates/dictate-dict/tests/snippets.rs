//! S24: snippet storage, migration 3, matching and variable expansion.
use dictate_dict::{
    expand_snippet, store, Dictionary, DictionaryConfig, DictionaryStore, NoVariables,
    SnippetVariables,
};
use dictate_proto::{AppContext, DictionaryEntry, ErrorCode, Snippet};
use rusqlite::Connection;

fn dict() -> Dictionary {
    Dictionary::new(
        DictionaryStore::in_memory().unwrap(),
        DictionaryConfig::default(),
        None,
    )
    .unwrap()
}
fn add(d: &Dictionary, trigger: &str, expansion: &str) -> Snippet {
    d.upsert_snippet(Snippet::new(trigger, expansion)).unwrap()
}
/// Applies every match to `text`, the way the chain stage does.
fn run(d: &Dictionary, text: &str) -> String {
    let mut out = text.to_string();
    for m in d.find_snippets(text, None).iter().rev() {
        out.replace_range(m.range.clone(), &expand_snippet(&m.template, &NoVariables));
    }
    out
}

// --- migration 3 ---------------------------------------------------------------

#[test]
fn migration_3_adds_the_snippets_table_and_keeps_every_dictionary_row() {
    // A database exactly as the shipped v2 daemon left it, with data.
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(store::MIGRATION_V1).unwrap();
    c.execute_batch(
        "ALTER TABLE entries ADD COLUMN apps TEXT NOT NULL DEFAULT '[]'; PRAGMA user_version=2; \
         INSERT INTO entries (phrase,phrase_key,sounds_like,case_sensitive,enabled,source,hit_count,apps,created_at,updated_at) \
         VALUES ('Tauri','tauri','[\"tow ree\"]',0,1,'manual',7,'[\"slack\"]',10,20), \
                ('Widget','widget','[]',1,0,'manual',0,'[]',30,40)",
    )
    .unwrap();
    let mut upgraded = DictionaryStore::from_connection(c).unwrap();

    let entries = upgraded.entries().unwrap();
    assert_eq!(entries.len(), 2, "no dictionary row may be lost");
    assert_eq!(entries[0].entry.phrase, "Tauri");
    assert_eq!(entries[0].entry.apps, vec!["slack"]);
    assert_eq!(entries[0].entry.hit_count, Some(7));
    assert_eq!((entries[0].created_at, entries[0].updated_at), (10, 20));
    assert!(entries[1].entry.case_sensitive && !entries[1].entry.enabled);
    assert!(upgraded.snippets().unwrap().is_empty());
    upgraded
        .upsert_snippet(Snippet::new("sig", "— Jake"))
        .unwrap();
}

#[test]
fn migration_3_is_forward_only_and_idempotent_across_reopens() {
    let dir = std::env::temp_dir().join(format!("snip-mig-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("dictionary.db");
    {
        let d = Dictionary::open(
            DictionaryConfig {
                db_path: path.display().to_string(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        d.upsert(DictionaryEntry::new("Tauri")).unwrap();
        d.upsert_snippet(Snippet::new("sig", "— Jake")).unwrap();
    }
    for _ in 0..2 {
        let s = DictionaryStore::open(&path).unwrap();
        assert_eq!(s.entries().unwrap().len(), 1);
        assert_eq!(
            s.snippets().unwrap().len(),
            1,
            "a reopen must not rerun or wipe it"
        );
    }
    let c = Connection::open(&path).unwrap();
    let v: u32 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, store::SCHEMA_VERSION);
    assert_eq!(v, 3);
    // Re-running the SQL itself (a half-applied upgrade) is harmless too.
    c.execute_batch(store::MIGRATION_V3).unwrap();
    let n: u32 = c
        .query_row("SELECT COUNT(*) FROM snippets", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    // A database from a newer daemon is still refused.
    c.execute_batch("PRAGMA user_version=4").unwrap();
    drop(c);
    assert!(DictionaryStore::open(&path).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

// --- CRUD ------------------------------------------------------------------------

#[test]
fn crud_normalises_uniqueness_and_owns_hit_counts() {
    let d = dict();
    let mut s = Snippet::new("My  Address", "1 Example Way\nTestville");
    s.hit_count = Some(99);
    s.apps = vec!["slack".into()];
    s.category = Some("personal".into());
    let saved = d.upsert_snippet(s).unwrap();
    assert!(saved.id.is_some());
    assert_eq!(saved.hit_count, Some(0), "hit counts are server-owned");

    // Same words, different spacing/case: the same trigger.
    assert_eq!(
        d.upsert_snippet(Snippet::new("my address", "x"))
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let mut edited = saved.clone();
    edited.expansion = "2 Example Way".into();
    edited.enabled = false;
    let edited = d.upsert_snippet(edited).unwrap();
    assert_eq!(d.list_snippets(None, None), vec![edited.clone()]);
    assert_eq!(d.list_snippets(Some("ADDR"), None).len(), 1);
    assert!(d.list_snippets(Some("nomatch"), None).is_empty());

    let mut ghost = Snippet::new("ghost", "boo");
    ghost.id = Some(4242);
    assert_eq!(
        d.upsert_snippet(ghost).unwrap_err().code,
        ErrorCode::NotFound
    );
    d.delete_snippet(edited.id.unwrap()).unwrap();
    assert_eq!(
        d.delete_snippet(edited.id.unwrap()).unwrap_err().code,
        ErrorCode::NotFound
    );
    assert!(d.list_snippets(None, None).is_empty());
}

#[test]
fn validation_rejects_unspeakable_triggers_and_unsafe_expansions() {
    let d = dict();
    for (trigger, expansion) in [
        ("", "x"),
        ("  sig", "x"),
        ("&&&", "x"),
        ("a\nb", "x"),
        ("sig", ""),
        ("sig", "bell\u{7}"),
        ("sig", "a\u{F0000}b"), // a protected-span placeholder character
        ("sig", "a\u{E000}b"),
    ] {
        let e = d
            .upsert_snippet(Snippet::new(trigger, expansion))
            .unwrap_err();
        assert_eq!(
            e.code,
            ErrorCode::InvalidParams,
            "{trigger:?} → {expansion:?}"
        );
    }
    // Newlines and tabs are the point of a multi-line snippet.
    d.upsert_snippet(Snippet::new("sig", "Best,\n\tJake"))
        .unwrap();
    let too_long = "x".repeat(dictate_dict::snippets::MAX_EXPANSION_CHARS + 1);
    assert!(d.upsert_snippet(Snippet::new("long", too_long)).is_err());
}

// --- matching ----------------------------------------------------------------------

#[test]
fn table_driven_matching() {
    let d = dict();
    add(&d, "work email", "jake@example.com");
    add(&d, "insert work email", "<<LONG>>");
    add(&d, "sig", "Best,\nJake");
    add(&d, "my address", "1 Example Way");
    add(&d, "off", "NEVER");
    d.upsert_snippet(Snippet {
        enabled: false,
        ..Snippet::new("disabled one", "NEVER")
    })
    .unwrap();

    let cases = [
        // (spoken, expected)
        ("insert work email", "<<LONG>>"),   // longest trigger wins
        ("Insert work email.", "<<LONG>>"),  // case + trailing period swallowed
        ("insert, work email!", "<<LONG>>"), // punctuation inside the trigger
        (
            "send it to work email please",
            "send it to jake@example.com please",
        ),
        (
            "send it to work email, please",
            "send it to jake@example.com, please",
        ),
        ("Work email.", "jake@example.com"),
        ("my address and sig", "1 Example Way and Best,\nJake"),
        ("my address, sig.", "1 Example Way, Best,\nJake"),
        ("my addresses", "my addresses"), // whole words only
        ("signal", "signal"),
        ("work", "work"),                 // a prefix of a trigger is not it
        ("disabled one", "disabled one"), // disabled snippets never fire
        ("nothing to see", "nothing to see"),
        ("work email work email", "jake@example.com jake@example.com"),
        ("É sig", "É Best,\nJake"),
    ];
    for (spoken, expected) in cases {
        assert_eq!(run(&d, spoken), expected, "spoken: {spoken:?}");
    }
}

#[test]
fn a_trigger_never_matches_across_a_placeholder_or_through_other_text() {
    let d = dict();
    add(&d, "work email", "jake@example.com");
    // U+F0000 is how the text chain stands in for a protected span.
    assert!(d.find_snippets("work \u{F0000} email", None).is_empty());
    assert!(d.find_snippets("work and email", None).is_empty());
    assert!(
        d.find_snippets("work\nemail", None).len() == 1,
        "whitespace separates words"
    );
}

#[test]
fn only_a_trailing_trigger_swallows_punctuation() {
    let d = dict();
    add(&d, "sig", "S");
    let m = d.find_snippets("see sig. then more", None);
    assert_eq!(
        &"see sig. then more"[m[0].range.clone()],
        "sig",
        "mid-text period stays"
    );
    let m = d.find_snippets("see sig ?", None);
    assert_eq!(
        m[0].range,
        4..7,
        "a detached mark after a space is not swallowed"
    );
}

#[test]
fn per_app_scope() {
    let d = dict();
    d.upsert_snippet(Snippet {
        apps: vec!["Slack".into()],
        ..Snippet::new("standup", "yesterday / today / blockers")
    })
    .unwrap();
    add(&d, "sig", "S");
    let slack = AppContext::new("slack");
    let term = AppContext::new("ghostty");
    assert_eq!(
        d.find_snippets("standup", Some(&slack)).len(),
        1,
        "case-insensitive app id"
    );
    assert!(d.find_snippets("standup", Some(&term)).is_empty());
    assert!(
        d.find_snippets("standup", None).is_empty(),
        "no context → global snippets only"
    );
    assert_eq!(d.find_snippets("sig", None).len(), 1);
    assert_eq!(d.find_snippets("sig", Some(&term)).len(), 1);
}

#[test]
fn a_disabled_dictionary_expands_nothing() {
    let d = Dictionary::new(
        DictionaryStore::in_memory().unwrap(),
        DictionaryConfig {
            enabled: false,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    add(&d, "sig", "S");
    assert!(d.find_snippets("sig", None).is_empty());
}

#[test]
fn hits_are_counted_unless_private_and_survive_a_flush() {
    let d = dict();
    let s = add(&d, "sig", "S");
    let m = d.find_snippets("sig and sig", None);
    d.record_snippet_hits(&m, true);
    d.flush_hits().unwrap();
    assert_eq!(
        d.list_snippets(None, None)[0].hit_count,
        Some(0),
        "privacy leaves no trace"
    );
    d.record_snippet_hits(&m, false);
    d.flush_hits().unwrap();
    assert_eq!(d.list_snippets(None, None)[0].hit_count, Some(2));
    assert_eq!(s.id, d.list_snippets(None, None)[0].id);
}

#[test]
fn dictionary_and_snippet_stores_do_not_interfere() {
    let d = dict();
    d.upsert(DictionaryEntry::new("Tauri")).unwrap();
    add(&d, "tauri", "framework"); // same words, separate namespaces
    assert_eq!(d.list(None, None).len(), 1);
    assert_eq!(d.list_snippets(None, None).len(), 1);
}

// --- variables -----------------------------------------------------------------------

struct Fixed;
impl SnippetVariables for Fixed {
    fn resolve(&self, name: &str) -> Option<String> {
        match name {
            "date" => Some("2026-01-02".into()),
            "time" => Some("09:30".into()),
            "clipboard" => Some("CLIP {selection}".into()),
            "selection" => Some("SEL".into()),
            _ => None,
        }
    }
}

#[test]
fn table_driven_variable_expansion() {
    let cases = [
        ("Dated {date} at {time}", "Dated 2026-01-02 at 09:30"),
        ("{selection}: {clipboard}", "SEL: CLIP {selection}"), // values are not re-expanded
        ("{{date}} stays literal", "{date} stays literal"),
        ("{unknown} and {Date} stay", "{unknown} and {Date} stay"), // unknown / wrong case
        ("{{unknown}}", "{{unknown}}"),
        ("json {\"a\": {\"b\": 1}}", "json {\"a\": {\"b\": 1}}"),
        ("{date}{date}", "2026-01-022026-01-02"),
        ("no variables", "no variables"),
        ("{}", "{}"),
        ("{ date }", "{ date }"),
    ];
    for (template, expected) in cases {
        assert_eq!(expand_snippet(template, &Fixed), expected, "{template:?}");
    }
    // A resolver that knows nothing leaves the placeholder as written.
    assert_eq!(expand_snippet("a {date} b", &NoVariables), "a {date} b");
}

#[test]
fn variables_are_resolved_only_when_the_template_uses_them() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting(AtomicUsize);
    impl SnippetVariables for Counting {
        fn resolve(&self, _: &str) -> Option<String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Some(String::new())
        }
    }
    let c = Counting(AtomicUsize::new(0));
    let _ = expand_snippet("plain text", &c);
    assert_eq!(
        c.0.load(Ordering::SeqCst),
        0,
        "clipboard must not be read needlessly"
    );
    let _ = expand_snippet("{clipboard}", &c);
    assert_eq!(c.0.load(Ordering::SeqCst), 1);
}
