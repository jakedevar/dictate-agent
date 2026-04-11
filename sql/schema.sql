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
    error_summary TEXT
);

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);
