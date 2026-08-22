CREATE TABLE IF NOT EXISTS interactions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    timestamp TEXT NOT NULL,

    audio_duration_s REAL,

    raw_transcription TEXT,
    corrected_transcription TEXT,
    transcription_duration_s REAL,

    grammar_input TEXT,
    grammar_output TEXT,
    grammar_changed INTEGER,
    grammar_error TEXT,
    grammar_duration_s REAL,

    route_type TEXT,
    route_model TEXT,
    route_trigger TEXT,
    route_confidence REAL,

    prompt_sent TEXT,
    response_text TEXT,
    execution_model TEXT,
    execution_duration_s REAL,
    execution_success INTEGER,
    execution_error TEXT,

    output_typed INTEGER,
    output_char_count INTEGER,

    total_duration_s REAL,
    completed INTEGER DEFAULT 0,
    error_summary TEXT,

    -- History v2. All stage values are milliseconds; NULL means that the
    -- stage was not measured, rather than pretending it took zero time.
    capture_duration_ms REAL,
    vad_duration_ms REAL,
    stt_duration_ms REAL,
    fmt_rules_duration_ms REAL,
    fmt_llm_duration_ms REAL,
    inject_duration_ms REAL,
    app_context TEXT,
    stt_model TEXT,
    word_count INTEGER
);

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);

-- External-content FTS keeps transcript search fast without duplicating the
-- rest of the interaction record. Triggers keep it correct for inserts,
-- imports, retention cleanup, and explicit purges.
CREATE VIRTUAL TABLE IF NOT EXISTS interactions_fts USING fts5(
    corrected_transcription,
    raw_transcription,
    content='interactions',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS interactions_ai AFTER INSERT ON interactions BEGIN
    INSERT INTO interactions_fts(rowid, corrected_transcription, raw_transcription)
    VALUES (new.id, new.corrected_transcription, new.raw_transcription);
END;

CREATE TRIGGER IF NOT EXISTS interactions_ad AFTER DELETE ON interactions BEGIN
    INSERT INTO interactions_fts(interactions_fts, rowid, corrected_transcription, raw_transcription)
    VALUES ('delete', old.id, old.corrected_transcription, old.raw_transcription);
END;

CREATE TRIGGER IF NOT EXISTS interactions_au AFTER UPDATE OF corrected_transcription, raw_transcription ON interactions BEGIN
    INSERT INTO interactions_fts(interactions_fts, rowid, corrected_transcription, raw_transcription)
    VALUES ('delete', old.id, old.corrected_transcription, old.raw_transcription);
    INSERT INTO interactions_fts(rowid, corrected_transcription, raw_transcription)
    VALUES (new.id, new.corrected_transcription, new.raw_transcription);
END;

CREATE TABLE IF NOT EXISTS history_imports (
    source_path TEXT PRIMARY KEY,
    imported_at TEXT NOT NULL,
    row_count INTEGER NOT NULL
);
