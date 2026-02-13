"""Interaction history storage for analytics."""

import sqlite3
import time
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional


# XDG-compliant default location
DEFAULT_DB_DIR = Path.home() / ".local" / "share" / "dictate-agent"
DEFAULT_DB_PATH = DEFAULT_DB_DIR / "history.db"

SCHEMA_VERSION = 1

CREATE_TABLE = """
CREATE TABLE IF NOT EXISTS interactions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    timestamp TEXT NOT NULL,

    -- Audio metadata
    audio_duration_s REAL,

    -- Transcription
    raw_transcription TEXT,
    corrected_transcription TEXT,
    transcription_duration_s REAL,

    -- Grammar correction
    grammar_input TEXT,
    grammar_output TEXT,
    grammar_changed INTEGER,
    grammar_error TEXT,
    grammar_duration_s REAL,

    -- Routing
    route_type TEXT,
    route_model TEXT,
    route_trigger TEXT,
    route_confidence REAL,

    -- Execution
    prompt_sent TEXT,
    response_text TEXT,
    execution_model TEXT,
    execution_duration_s REAL,
    execution_success INTEGER,
    execution_error TEXT,

    -- Output
    output_typed INTEGER,
    output_char_count INTEGER,

    -- Pipeline totals
    total_duration_s REAL,
    completed INTEGER DEFAULT 0,
    error_summary TEXT
);

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);
"""


@dataclass
class Interaction:
    """Builder for a single interaction record."""

    session_id: str = ""
    timestamp: str = ""
    _start_time: float = 0.0

    # Audio
    audio_duration_s: Optional[float] = None

    # Transcription
    raw_transcription: Optional[str] = None
    corrected_transcription: Optional[str] = None
    transcription_duration_s: Optional[float] = None

    # Grammar
    grammar_input: Optional[str] = None
    grammar_output: Optional[str] = None
    grammar_changed: Optional[int] = None
    grammar_error: Optional[str] = None
    grammar_duration_s: Optional[float] = None

    # Routing
    route_type: Optional[str] = None
    route_model: Optional[str] = None
    route_trigger: Optional[str] = None
    route_confidence: Optional[float] = None

    # Execution
    prompt_sent: Optional[str] = None
    response_text: Optional[str] = None
    execution_model: Optional[str] = None
    execution_duration_s: Optional[float] = None
    execution_success: Optional[int] = None
    execution_error: Optional[str] = None

    # Output
    output_typed: Optional[int] = None
    output_char_count: Optional[int] = None

    # Pipeline
    total_duration_s: Optional[float] = None
    completed: int = 0
    error_summary: Optional[str] = None


class HistoryStore:
    """SQLite-backed interaction history."""

    def __init__(self, db_path: Optional[Path] = None):
        self.db_path = db_path or DEFAULT_DB_PATH
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self._session_id = uuid.uuid4().hex[:12]
        self._conn: Optional[sqlite3.Connection] = None
        self._init_db()

    def _init_db(self) -> None:
        self._conn = sqlite3.connect(str(self.db_path))
        self._conn.execute("PRAGMA journal_mode=WAL")
        self._conn.executescript(CREATE_TABLE)
        # Set schema version if not present
        cursor = self._conn.execute(
            "SELECT version FROM schema_version LIMIT 1"
        )
        if cursor.fetchone() is None:
            self._conn.execute(
                "INSERT INTO schema_version (version) VALUES (?)",
                (SCHEMA_VERSION,),
            )
        self._conn.commit()

    def begin(self) -> Interaction:
        """Start tracking a new interaction."""
        return Interaction(
            session_id=self._session_id,
            timestamp=datetime.now(timezone.utc).isoformat(),
            _start_time=time.monotonic(),
        )

    def commit(self, interaction: Interaction) -> None:
        """Write a completed interaction to the database."""
        if self._conn is None:
            return

        interaction.total_duration_s = (
            time.monotonic() - interaction._start_time
        )

        self._conn.execute(
            """INSERT INTO interactions (
                session_id, timestamp,
                audio_duration_s,
                raw_transcription, corrected_transcription, transcription_duration_s,
                grammar_input, grammar_output, grammar_changed, grammar_error, grammar_duration_s,
                route_type, route_model, route_trigger, route_confidence,
                prompt_sent, response_text, execution_model, execution_duration_s,
                execution_success, execution_error,
                output_typed, output_char_count,
                total_duration_s, completed, error_summary
            ) VALUES (
                ?, ?,
                ?,
                ?, ?, ?,
                ?, ?, ?, ?, ?,
                ?, ?, ?, ?,
                ?, ?, ?, ?,
                ?, ?,
                ?, ?,
                ?, ?, ?
            )""",
            (
                interaction.session_id, interaction.timestamp,
                interaction.audio_duration_s,
                interaction.raw_transcription, interaction.corrected_transcription,
                interaction.transcription_duration_s,
                interaction.grammar_input, interaction.grammar_output,
                interaction.grammar_changed, interaction.grammar_error,
                interaction.grammar_duration_s,
                interaction.route_type, interaction.route_model,
                interaction.route_trigger, interaction.route_confidence,
                interaction.prompt_sent, interaction.response_text,
                interaction.execution_model, interaction.execution_duration_s,
                interaction.execution_success, interaction.execution_error,
                interaction.output_typed, interaction.output_char_count,
                interaction.total_duration_s, interaction.completed,
                interaction.error_summary,
            ),
        )
        self._conn.commit()

    def close(self) -> None:
        """Close the database connection."""
        if self._conn:
            self._conn.close()
            self._conn = None
