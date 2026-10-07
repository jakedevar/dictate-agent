use dictate_dict::{Dictionary, DictionaryConfig, DictionaryStore};
use dictate_proto::{AppContext, DictionaryEntry, EntrySource, ErrorCode};
use rusqlite::Connection;

fn dict(config: DictionaryConfig) -> Dictionary {
    Dictionary::new(DictionaryStore::in_memory().unwrap(), config, None).unwrap()
}
fn entry(phrase: &str, aliases: &[&str]) -> DictionaryEntry {
    let mut e = DictionaryEntry::new(phrase);
    e.sounds_like = aliases.iter().map(|s| s.to_string()).collect();
    e
}
#[test]
fn migrations_empty_v1_and_future_version() {
    let empty = DictionaryStore::in_memory().unwrap();
    assert!(empty.entries().unwrap().is_empty());
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(dictate_dict::store::MIGRATION_V1).unwrap();
    c.execute_batch("PRAGMA user_version=1; INSERT INTO entries (phrase,phrase_key,sounds_like,case_sensitive,enabled,source,created_at,updated_at) VALUES ('Tauri','tauri','[\"tow ree\"]',0,1,'manual',10,20)").unwrap();
    let upgraded = DictionaryStore::from_connection(c).unwrap();
    let e = &upgraded.entries().unwrap()[0];
    assert_eq!(e.entry.phrase, "Tauri");
    assert!(e.entry.apps.is_empty());
    assert_eq!((e.created_at, e.updated_at), (10, 20));
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA user_version=99").unwrap();
    assert!(DictionaryStore::from_connection(c).is_err());
}
#[test]
fn crud_validates_unicode_uniqueness_and_server_owned_hits() {
    let d = dict(Default::default());
    let mut e = entry("Éclair", &["eclaire"]);
    e.hit_count = Some(999);
    e.apps = vec!["slack".into()];
    let saved = d.upsert(e).unwrap();
    assert_eq!(saved.hit_count, Some(0));
    assert_eq!(
        d.upsert(entry("éclair", &[])).unwrap_err().code,
        ErrorCode::Conflict
    );
    let mut update = saved.clone();
    update.enabled = false;
    update.hit_count = Some(888);
    assert_eq!(d.upsert(update).unwrap().hit_count, Some(0));
    d.delete(saved.id.unwrap()).unwrap();
    assert!(d.list(None, None).is_empty());
    assert_eq!(
        d.delete(saved.id.unwrap()).unwrap_err().code,
        ErrorCode::NotFound
    );
    for phrase in ["", "  bad", "bad\nterm"] {
        assert_eq!(
            d.upsert(entry(phrase, &[])).unwrap_err().code,
            ErrorCode::InvalidParams
        );
    }
    assert_eq!(
        d.upsert(DictionaryEntry {
            id: Some(900),
            ..entry("Absent", &[])
        })
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
}
#[test]
fn exact_boundaries_possessives_and_output_ranges() {
    let d = dict(Default::default());
    d.upsert(entry("Kubernetes", &["kubernetties", "cube ernetties"]))
        .unwrap();
    d.upsert(entry("Éclair", &["eclaire"])).unwrap();
    for (input, expected) in [
        (
            "(KUBERNETTIES), cube ernetties!",
            "(Kubernetes), Kubernetes!",
        ),
        (
            "Kubernetties's and kubernetties’s",
            "Kubernetes's and Kubernetes’s",
        ),
        (
            "kubernettiesness _kubernetties kubernetties_",
            "kubernettiesness _kubernetties kubernetties_",
        ),
        ("ékubernetties kubernettiesé", "ékubernetties kubernettiesé"),
        ("kubernetties\u{301}", "kubernetties\u{301}"),
        ("eclaire kubernetes", "Éclair Kubernetes"),
    ] {
        let a = d.apply(input, None);
        assert_eq!(a.text, expected);
        for r in a.replacements {
            assert_eq!(&a.text[r.range], r.after);
        }
    }
}
#[test]
fn overlaps_longest_first_and_ambiguous_aliases_declined() {
    let d = dict(Default::default());
    d.upsert(entry("Short", &["blue"])).unwrap();
    d.upsert(entry("Long", &["blue widget"])).unwrap();
    assert_eq!(d.apply("blue widget blue", None).text, "Long Short");
    d.upsert(entry("Other", &["blue"])).unwrap();
    assert_eq!(d.apply("blue", None).text, "blue");
}
#[test]
fn case_scope_disabled_and_recase_switch() {
    let d = dict(Default::default());
    let mut e = entry("API", &["Apii"]);
    e.case_sensitive = true;
    e.apps = vec!["SLACK".into()];
    d.upsert(e).unwrap();
    assert_eq!(d.apply("Apii apii api", None).text, "Apii apii api");
    assert_eq!(
        d.apply("Apii apii api", Some(&AppContext::new("slack")))
            .text,
        "API apii api"
    );
    let d = dict(DictionaryConfig {
        recase_phrases: false,
        ..Default::default()
    });
    d.upsert(entry("Tauri", &["tow ree"])).unwrap();
    assert_eq!(d.apply("tauri tow ree", None).text, "tauri Tauri");
    let mut e = d.list(None, None)[0].clone();
    e.enabled = false;
    d.upsert(e).unwrap();
    assert_eq!(d.apply("tow ree", None).text, "tow ree");
}
#[test]
fn unicode_case_expansion_is_safe() {
    let d = dict(Default::default());
    d.upsert(entry("Term", &["İSTANBUL"])).unwrap();
    assert_eq!(d.apply("İstanbul!", None).text, "Term!");
    assert_eq!(d.apply("xİstanbul", None).text, "xİstanbul");
}
#[test]
fn prompt_ranking_budget_scope_and_opt_out() {
    let mut store = DictionaryStore::in_memory().unwrap();
    let manual = store.upsert(entry("Manual", &[])).unwrap();
    let mut automatic = entry("Popular", &[]);
    automatic.source = EntrySource::AutoLearned;
    let automatic = store.upsert(automatic).unwrap();
    store
        .increment_hits(&[(automatic.id.unwrap(), 50), (manual.id.unwrap(), 1)].into())
        .unwrap();
    let d = Dictionary::new(
        store,
        DictionaryConfig::default(),
        Some("Static hint.".into()),
    )
    .unwrap();
    assert_eq!(
        d.initial_prompt(None, true).unwrap(),
        "Static hint. Glossary: Manual, Popular."
    );
    assert_eq!(d.initial_prompt(None, false).unwrap(), "Static hint.");
    let mut local = entry("Scoped", &[]);
    local.apps = vec!["slack".into()];
    d.upsert(local).unwrap();
    assert!(!d.vocabulary(None).contains(&"Scoped".into()));
    assert!(d
        .vocabulary(Some(&AppContext::new("Slack")))
        .contains(&"Scoped".into()));
    let d = Dictionary::new(
        DictionaryStore::in_memory().unwrap(),
        DictionaryConfig {
            max_prompt_chars: 900,
            ..Default::default()
        },
        Some("é".repeat(500)),
    )
    .unwrap();
    assert_eq!(d.initial_prompt(None, true).unwrap().len(), 400);
    let d = Dictionary::new(
        DictionaryStore::in_memory().unwrap(),
        DictionaryConfig {
            max_prompt_chars: 25,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    d.upsert(entry("Kubernetes", &[])).unwrap();
    d.upsert(entry("ExtraLongVocabularyTerm", &[])).unwrap();
    assert_eq!(
        d.initial_prompt(None, true).unwrap(),
        "Glossary: Kubernetes."
    );
}
#[test]
fn private_sessions_never_increment_and_normal_hits_are_batched() {
    let d = dict(Default::default());
    d.upsert(entry("Tauri", &["tow ree"])).unwrap();
    let a = d.apply("tow ree tow ree", None);
    d.record_hits(&a.replacements, true);
    d.flush_hits().unwrap();
    assert_eq!(d.list(None, None)[0].hit_count, Some(0));
    d.record_hits(&a.replacements, false);
    assert_eq!(d.list(None, None)[0].hit_count, Some(0));
    d.flush_hits().unwrap();
    assert_eq!(d.list(None, None)[0].hit_count, Some(2));
    assert!(d.apply("Tauri", None).replacements.is_empty());
}
#[test]
fn precision_corpus_330_sentences_default_matching_changes_nothing() {
    let d = dict(Default::default());
    for e in [
        entry("Kubernetes", &["kubernetties"]),
        entry("Tauri", &["tow ree"]),
        entry("Claude", &["clawd"]),
    ] {
        d.upsert(e).unwrap();
    }
    let subjects = [
        "The reader",
        "A child",
        "This old machine",
        "Our example",
        "A careful writer",
        "The server",
        "One project",
        "That new tool",
        "The queue",
        "An editor",
        "The browser",
    ];
    let actions = [
        "works with",
        "does not need",
        "keeps",
        "tests",
        "uses",
        "reviews",
    ];
    let objects = [
        "ordinary words.",
        "a clear cloud.",
        "the tauriish widget.",
        "kubernettiesness.",
        "a towel and a tree.",
    ];
    let mut count = 0;
    for s in subjects {
        for a in actions {
            for o in objects {
                let input = format!("{s} {a} {o}");
                assert_eq!(d.apply(&input, None).text, input);
                count += 1;
            }
        }
    }
    assert_eq!(count, 330);
    assert!(!d.config.fuzzy);
}
#[test]
fn fuzzy_opt_in_single_token_threshold_and_ambiguity() {
    let d = dict(DictionaryConfig {
        fuzzy: true,
        fuzzy_threshold: 0.9,
        ..Default::default()
    });
    d.upsert(entry("Kubernetes", &[])).unwrap();
    assert_eq!(d.apply("Kubernetez", None).text, "Kubernetes");
    assert_eq!(d.apply("a cube and a net", None).text, "a cube and a net");
    d.upsert(entry("Kuberneteq", &[])).unwrap();
    assert_eq!(d.apply("Kubernetez", None).text, "Kubernetez");
}

#[test]
fn persistent_wal_store_reopens_without_losing_metadata() {
    let path = std::env::temp_dir().join(format!("s22-dictionary-{}.db", std::process::id()));
    let saved;
    {
        let mut store = DictionaryStore::open(&path).unwrap();
        saved = store.upsert(entry("Tauri", &["tow ree"])).unwrap();
        store
            .increment_hits(&[(saved.id.unwrap(), 3)].into())
            .unwrap();
    }
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        3 // S24 added migration 3 (snippets)
    );
    drop(conn);
    {
        let store = DictionaryStore::open(&path).unwrap();
        let e = &store.entries().unwrap()[0];
        assert_eq!(e.entry.id, saved.id);
        assert_eq!(e.entry.hit_count, Some(3));
        assert!(e.created_at > 0);
        assert!(e.updated_at >= e.created_at);
    }
    std::fs::remove_file(path).unwrap();
}
#[test]
fn case_sensitive_entries_can_explicitly_allow_multiple_case_variants() {
    let d = dict(Default::default());
    let mut e = entry("API", &["Apii", "APII"]);
    e.case_sensitive = true;
    d.upsert(e).unwrap();
    assert_eq!(d.apply("Apii APII apii", None).text, "API API apii");
}

#[test]
fn unicode_casefold_sigma_and_sharp_s_respect_boundaries_and_uniqueness() {
    let d = dict(Default::default());
    d.upsert(entry("GreekTerm", &["ΟΣ"])).unwrap();
    d.upsert(entry("GermanTerm", &["STRASSE"])).unwrap();
    assert_eq!(
        d.apply("ος οσ ΟΣ Straße", None).text,
        "GreekTerm GreekTerm GreekTerm GermanTerm"
    );
    d.upsert(entry("ΟΣ", &[])).unwrap();
    assert_eq!(
        d.upsert(entry("ος", &[])).unwrap_err().code,
        ErrorCode::Conflict
    );
    let d = dict(Default::default());
    d.upsert(entry("S", &[])).unwrap();
    assert_eq!(
        d.apply("ß", None).text,
        "ß",
        "partial expanded character must not match"
    );
}
/// `matcher-contraction-inside-word`: an apostrophe or hyphen between
/// letters is inside a word, so no entry matches a piece of a contraction or
/// a hyphenated compound — while possessives still match and recase.
#[test]
fn contractions_and_hyphenated_compounds_are_never_split() {
    let d = dict(Default::default()); // recase_phrases = true
    for e in [
        entry("Don", &[]),
        entry("Won", &[]),
        entry("Cant", &["can"]),
        entry("Mail", &[]),
        entry("Tauri", &["tow ree"]),
    ] {
        d.upsert(e).unwrap();
    }
    // Must not change.
    for input in [
        "I don't know.",
        "I DON'T know.",
        "I don\u{2019}t know.",
        "I can't go.",
        "I can\u{2019}t go.",
        "It won't work.",
        "Send an e-mail.",
        "Send e-mails and e-mail's drafts.",
        "the tow-ree thing",
        "rock'n'roll and o'won",
    ] {
        assert_eq!(d.apply(input, None).text, input, "input: {input:?}");
    }
    // Still matched: whole words, possessives, and a trailing quote or dash.
    for (input, want) in [
        ("ask don about it", "ask Don about it"),
        (
            "don's car and don\u{2019}s bike",
            "Don's car and Don\u{2019}s bike",
        ),
        ("he said 'don' twice", "he said 'Don' twice"),
        ("won - barely", "Won - barely"),
        ("I can go", "I Cant go"),
        ("tow ree's docs", "Tauri's docs"),
    ] {
        assert_eq!(d.apply(input, None).text, want, "input: {input:?}");
    }
}
/// `recase-common-word-phrases`: with `recase_phrases` on, an entry whose
/// phrase is an ordinary English word must not recase that word everywhere
/// ("rust" on a bumper), while a distinctive term still recases and an
/// explicit `sounds_like` alias is always honoured.
#[test]
fn recasing_skips_ordinary_words_but_not_terms_or_explicit_aliases() {
    let d = dict(Default::default()); // recase_phrases = true
    for e in [
        entry("Rust", &[]),
        entry("Swift", &[]),
        entry("Kubernetes", &[]),
        entry("Tauri", &[]),
        entry("Apple", &["appel"]),
    ] {
        d.upsert(e).unwrap();
    }
    // Ordinary words are left alone.
    for input in [
        "the rust on the car",
        "she was swift to reply",
        "an apple a day",
        "Rust is a language",
    ] {
        assert_eq!(d.apply(input, None).text, input, "input: {input:?}");
    }
    // Terms recase; an explicit alias still rewrites.
    for (input, want) in [
        ("deploy to kubernetes now", "deploy to Kubernetes now"),
        ("try tauri", "try Tauri"),
        ("I like appel pie", "I like Apple pie"),
    ] {
        assert_eq!(d.apply(input, None).text, want, "input: {input:?}");
    }
    // Opting out of the rule: recase_phrases off never recased anyway, and a
    // multi-word phrase containing a rare word is not an ordinary word.
    d.upsert(entry("Rust Belt", &[])).unwrap();
    assert_eq!(d.apply("the rust belt", None).text, "the Rust Belt");
}
