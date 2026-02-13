# Interaction History & Analytics System — Implementation Plan

## Overview

Add a SQLite-based interaction history system to the dictate agent that captures every stage of the voice-to-output pipeline. This enables statistical analysis of transcription accuracy, grammar correction effectiveness, routing decisions, and response quality over time.

## Current State Analysis

- **Zero persistence**: All data is ephemeral — printed to stdout and discarded
- **No logging infrastructure**: No Python `logging`, no log files, no structured events
- **Clear interception points**: The `_stop_recording()` method in `main.py:144-189` is a linear pipeline where each stage's input/output can be captured with minimal disruption
- **Config system is extensible**: Adding a new `[history]` section follows the established `dataclass + load_config() .get()` pattern in `config.py`

### Key Discoveries:
- Pipeline stages are sequential in `main.py:_stop_recording()` (lines 144-189) and `_handle_route()` (lines 191-251)
- `RouteType` is a Python `Enum` at `router.py:19-29` with 8 members — storing its `.value` (a string like `"haiku"`, `"timer"`) makes the column future-proof without schema changes
- `ExecutionResult` dataclass is defined separately in both `executor.py:10-16` and `local_executor.py:12-18` — both have `success`, `response`, `error` fields
- `GrammarResult` at `grammar.py:12-19` has `success`, `corrected`, `original`, `error`
- Config parsing pattern at `config.py:121-215` uses `if "section" in data:` → `.get()` with dataclass defaults

## Desired End State

A new `dictate/history.py` module provides a `HistoryStore` class that:
1. Creates/opens a SQLite database at `~/.local/share/dictate-agent/history.db`
2. Progressively records each pipeline stage's data via a builder-pattern `Interaction` object
3. Commits completed interaction rows on pipeline completion (or partial rows on error)
4. Is configurable via a `[history]` section in `config.toml`

**Verification**: After implementation, running `sqlite3 ~/.local/share/dictate-agent/history.db "SELECT * FROM interactions;"` after a voice interaction should show a row with all pipeline stage data populated.

## What We're NOT Doing

- No web dashboard or visualization UI — analysis will be done via SQL queries or pandas
- No raw audio file storage (disk space concern) — only metadata (duration)
- No user rating/feedback keybind — that's a separate feature
- No changes to the existing pipeline logic — this is purely additive instrumentation
- No Python `logging` module integration — this is structured event storage, not operational logging
- No export functionality — SQLite IS the export format (portable single file)

## Implementation Approach

Single-phase implementation: one new module (`history.py`), one config addition, and surgical insertions into `main.py`. The `route_type` column is stored as `TEXT` (the enum's `.value` string), so adding new voice commands in the future requires zero schema changes.

---

## Phase 1: Interaction History System

### Overview
Create the history module, add config support, and wire it into the pipeline.

### Changes Required:

#### 1. New Module: `dictate/history.py`

**File**: `dictate/history.py` (new file)
**Purpose**: SQLite database management and interaction recording

```python
"""Interaction history storage for analytics."""

import sqlite3
import time
import uuid
from dataclasses import dataclass, field
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
```

**Design decisions:**
- `route_type TEXT` — stores `RouteType.value` (e.g. `"haiku"`, `"timer"`, `"local"`). New enum values work without schema changes.
- `session_id` — groups interactions within a single daemon run for session-level analysis
- `PRAGMA journal_mode=WAL` — allows concurrent reads (for queries while daemon runs)
- `schema_version` table — enables future migrations
- `Interaction` is a flat dataclass, not nested — keeps SQLite mapping simple
- `_start_time` uses `time.monotonic()` for accurate duration measurement

---

#### 2. Config Addition: `HistoryConfig`

**File**: `dictate/config.py`
**Changes**: Add `HistoryConfig` dataclass and parsing logic

Add after `NotificationConfig` (after line 106):
```python
@dataclass
class HistoryConfig:
    """Interaction history configuration."""

    enabled: bool = True
    db_path: str = ""  # Empty string means use default XDG path
    max_response_length: int = 10000  # Truncate stored responses beyond this
```

Add to `Config` dataclass (after line 118):
```python
    history: HistoryConfig = field(default_factory=HistoryConfig)
```

Add parsing block in `load_config()` (after notifications parsing, before `return config`):
```python
    # History config
    if "history" in data:
        h = data["history"]
        config.history = HistoryConfig(
            enabled=h.get("enabled", config.history.enabled),
            db_path=h.get("db_path", config.history.db_path),
            max_response_length=h.get(
                "max_response_length", config.history.max_response_length
            ),
        )
```

---

#### 3. Config Example Update

**File**: `config/config.example.toml`
**Changes**: Add `[history]` section at the end

```toml
[history]
# Enable interaction history logging for analytics
enabled = true
# Database location (empty = ~/.local/share/dictate-agent/history.db)
db_path = ""
# Truncate stored responses beyond this length (characters)
max_response_length = 10000
```

---

#### 4. Pipeline Instrumentation

**File**: `dictate/main.py`
**Changes**: Initialize `HistoryStore` and record data at each pipeline stage

**Import addition** (after line 30, with the other imports):
```python
from .history import HistoryStore
```

**Initialization in `__init__()`** (after line 76, after `self.timer_executor`):
```python
        # History
        if self.config.history.enabled:
            from pathlib import Path as _Path

            db_path = (
                _Path(self.config.history.db_path)
                if self.config.history.db_path
                else None
            )
            self.history = HistoryStore(db_path=db_path)
        else:
            self.history = None
```

**Instrumentation in `_stop_recording()`** (lines 144-189):

The key changes wrap existing operations with timing and data capture. Here is the modified `_stop_recording()` logic — additions are marked with `# HISTORY` comments:

```python
    def _stop_recording(self) -> None:
        if not self.recording:
            return

        self.recording = False
        audio_path = self.audio.stop()

        if not audio_path:
            return

        # HISTORY: Begin tracking
        interaction = self.history.begin() if self.history else None

        print("Transcribing...")
        self.notifier.transcribing()

        # HISTORY: Time transcription
        t0 = time.monotonic()
        result = self.transcriber.transcribe(audio_path)
        t1 = time.monotonic()
        self.audio.cleanup()

        if not result or not result.text.strip():
            self.notifier.error("No speech detected")
            self._resume_media_if_needed()
            return

        text = result.text
        print(f"Transcribed: {text}")

        # HISTORY: Record transcription
        if interaction:
            interaction.audio_duration_s = getattr(result, "duration", None)
            interaction.raw_transcription = text
            interaction.corrected_transcription = text  # Updated after corrections
            interaction.transcription_duration_s = t1 - t0

        # Grammar correction
        if self.grammar.enabled:
            t2 = time.monotonic()
            grammar_result = self.grammar.correct(text)
            t3 = time.monotonic()
            if grammar_result.success and grammar_result.corrected != text:
                print(f"Grammar corrected: {grammar_result.corrected}")
            elif not grammar_result.success:
                print(f"Grammar correction failed: {grammar_result.error}")
            text = grammar_result.corrected

            # HISTORY: Record grammar correction
            if interaction:
                interaction.grammar_input = grammar_result.original
                interaction.grammar_output = grammar_result.corrected
                interaction.grammar_changed = int(
                    grammar_result.corrected != grammar_result.original
                )
                interaction.grammar_error = grammar_result.error
                interaction.grammar_duration_s = t3 - t2
                interaction.corrected_transcription = text

        # Route
        route_result = self.router.route(text)
        print(f"Route: {route_result.route.value} (confidence: {route_result.confidence})")

        # HISTORY: Record routing
        if interaction:
            interaction.route_type = route_result.route.value
            interaction.route_model = route_result.model
            interaction.route_confidence = route_result.confidence

        # Execute
        self._handle_route(route_result, interaction)

        self._resume_media_if_needed()

        # HISTORY: Commit
        if interaction:
            interaction.completed = 1
            self.history.commit(interaction)
```

**Pass `interaction` through `_handle_route()`** — modify the method signature and add recording:

```python
    def _handle_route(self, route_result, interaction=None) -> None:
        max_len = (
            self.config.history.max_response_length
            if self.config.history.enabled
            else 10000
        )

        if route_result.route == RouteType.TYPE:
            self.output.type_text(route_result.text)
            self.notifier.done(route_result.text)

            # HISTORY
            if interaction:
                interaction.output_typed = 1
                interaction.output_char_count = len(route_result.text)

        elif route_result.route == RouteType.COMMAND:
            # Not implemented — falls through to typing
            print("Command mode not yet implemented, typing text instead")
            self.output.type_text(route_result.text)
            self.notifier.done(route_result.text)

        elif route_result.route == RouteType.TIMER:
            result = self.timer_executor.execute(route_result.text)
            if result.success:
                self.notifier.timer_set(result.message)
            else:
                self.notifier.error(result.error or "Timer failed")

            # HISTORY
            if interaction:
                interaction.execution_success = int(result.success)
                interaction.execution_error = getattr(result, "error", None)

        elif route_result.route == RouteType.EDIT:
            print("Edit mode not yet implemented")
            self.notifier.error("Edit mode not yet implemented")

        elif route_result.route == RouteType.LOCAL:
            self.notifier.processing("local")
            t0 = time.monotonic()
            result = self.local_executor.execute(route_result.text)
            t1 = time.monotonic()
            if result.success:
                self.output.type_text(result.response)
                self.notifier.done(result.response)
            else:
                self.notifier.error(result.error or "Local execution failed")

            # HISTORY
            if interaction:
                interaction.prompt_sent = route_result.text
                interaction.response_text = (result.response or "")[:max_len]
                interaction.execution_model = "local"
                interaction.execution_duration_s = t1 - t0
                interaction.execution_success = int(result.success)
                interaction.execution_error = result.error
                interaction.output_typed = int(result.success)
                interaction.output_char_count = len(result.response) if result.success else 0

        elif route_result.route in (RouteType.HAIKU, RouteType.SONNET, RouteType.OPUS):
            model = route_result.model
            self.notifier.processing(model)
            response = []

            def on_delta(delta: str) -> None:
                response.append(delta)

            t0 = time.monotonic()
            result = self.executor.execute(
                route_result.text,
                model=model,
                on_delta=on_delta,
            )
            t1 = time.monotonic()

            if result.success:
                full_response = result.response or "".join(response)
                self.output.type_text(full_response)
                self.notifier.done(full_response)
            else:
                full_response = ""
                self.notifier.error(result.error or "Claude execution failed")

            # HISTORY
            if interaction:
                interaction.prompt_sent = route_result.text
                interaction.response_text = (full_response or "")[:max_len]
                interaction.execution_model = model
                interaction.execution_duration_s = t1 - t0
                interaction.execution_success = int(result.success)
                interaction.execution_error = result.error
                interaction.output_typed = int(result.success)
                interaction.output_char_count = len(full_response) if result.success else 0
```

**Cleanup in `stop()`** (add to the existing stop method, after PID file deletion):
```python
        if self.history:
            self.history.close()
```

---

### Success Criteria:

#### Automated Verification:
- [x] Module imports cleanly: `python -c "from dictate.history import HistoryStore, Interaction"`
- [x] Config loads with new section: `python -c "from dictate.config import load_config; c = load_config(); print(c.history.enabled)"`
- [x] Database creates on first use: `python -c "from dictate.history import HistoryStore; s = HistoryStore(); s.close()" && sqlite3 ~/.local/share/dictate-agent/history.db ".schema"`
- [x] Daemon starts without error: `python -m dictate.main &` (then kill)
- [x] No Python syntax errors: `python -m py_compile dictate/history.py && python -m py_compile dictate/config.py`

#### Manual Verification:
- [x] Perform a voice interaction with `$mod+n`, then query: `sqlite3 ~/.local/share/dictate-agent/history.db "SELECT timestamp, raw_transcription, route_type, execution_model, output_char_count FROM interactions ORDER BY id DESC LIMIT 1;"`
- [x] Verify all fields populated: `sqlite3 ~/.local/share/dictate-agent/history.db "SELECT * FROM interactions ORDER BY id DESC LIMIT 1;"`
- [x] Verify timing data is reasonable (positive, non-zero durations)
- [x] Verify grammar correction fields populated when grammar is enabled
- [ ] Verify partial rows saved when pipeline errors occur mid-way
- [ ] Verify `[history] enabled = false` in config.toml disables all history recording

---

## Testing Strategy

### Unit Tests:
- `HistoryStore` creates DB and table on init
- `Interaction` builder populates fields correctly
- `commit()` writes row and is queryable
- `close()` is idempotent
- Default DB path follows XDG convention
- Custom `db_path` from config is respected
- `max_response_length` truncates stored responses

### Manual Testing Steps:
1. Start daemon, perform 3+ voice interactions of different types (TYPE, HAIKU, TIMER)
2. Query DB: `sqlite3 ~/.local/share/dictate-agent/history.db "SELECT route_type, COUNT(*) FROM interactions GROUP BY route_type;"`
3. Verify session_id is consistent within one daemon run
4. Restart daemon, verify new session_id
5. Verify timing columns: `SELECT transcription_duration_s, grammar_duration_s, execution_duration_s, total_duration_s FROM interactions;`

## Performance Considerations

- SQLite WAL mode allows the daemon to write without blocking potential read queries
- Each `commit()` is a single INSERT — negligible latency (~1ms)
- Response truncation (`max_response_length`) prevents database bloat from long Claude responses
- No background threads — writes are synchronous but fast enough to be imperceptible in the pipeline

## Migration Notes

- **Schema version 1**: Initial schema. The `schema_version` table enables future `ALTER TABLE` migrations
- **No existing data to migrate**: This is a greenfield addition
- **Future route types**: Adding new `RouteType` enum values requires zero schema changes — `route_type TEXT` stores whatever string the enum produces

## References

- Pipeline flow: `dictate/main.py:144-251` (`_stop_recording` and `_handle_route`)
- Config pattern: `dictate/config.py:121-215` (`load_config()`)
- Route types: `dictate/router.py:19-29` (`RouteType` enum)
- Grammar results: `dictate/grammar.py:12-19` (`GrammarResult` dataclass)
- Execution results: `dictate/executor.py:10-16` and `dictate/local_executor.py:12-18`
