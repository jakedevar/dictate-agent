//! Reading the interaction log back out as protocol records.
//!
//! The `interactions` table was designed as a debugging journal for the Python
//! daemon, and its columns are the pipeline's internals: `grammar_duration_s`,
//! `route_trigger`, `execution_model`. [`dictate_proto::HistoryEntry`] is the
//! *public* shape of a past dictation. This module is the translation between
//! them, and it is where the honesty rules about that translation live.
//!
//! Two of those rules are load-bearing:
//!
//! - **Absent text is not empty text.** A row whose `corrected_transcription`
//!   is `NULL` — privacy mode, or a session that failed before producing
//!   anything — maps to `text: None`. A row holding `""` means the user said
//!   nothing, and maps to `Some("")`. Collapsing the two would make the
//!   history UI claim a private session was silent.
//! - **Stages that were never measured stay unmeasured.** The old schema
//!   records three durations, so only three stages can be reconstructed. The
//!   rest are `NotReported` rather than `Ran{0.0}`, because a row written by
//!   the Python daemon genuinely has no data for them.
//!
//! S30 owns the real analytics surface (FTS5, aggregates, per-app breakdowns).
//! This is the read path S02 needs to answer `query_history` truthfully.

use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use dictate_proto::{
    ErrorCode, HistoryEntry, HistoryPage, HistoryQuery, ProtoError, Route, SessionId, SortOrder,
    StageTiming, StageTimings,
};
use rusqlite::{Connection, Row};

/// Rows returned when the caller does not ask for a limit.
const DEFAULT_LIMIT: u32 = 50;
/// Ceiling on rows per request, so one query cannot pull the whole journal
/// into a single protocol message.
const MAX_LIMIT: u32 = 500;

/// Columns selected, in the order [`map_row`] reads them.
const COLUMNS: &str = "interactions.id, interactions.session_id, interactions.timestamp, \
                       interactions.corrected_transcription, interactions.raw_transcription, \
                       interactions.route_type, interactions.transcription_duration_s, \
                       interactions.grammar_duration_s, interactions.grammar_error, \
                       interactions.total_duration_s, interactions.audio_duration_s, interactions.error_summary, \
                       interactions.capture_duration_ms, interactions.vad_duration_ms, interactions.stt_duration_ms, \
                       interactions.fmt_rules_duration_ms, interactions.fmt_llm_duration_ms, \
                       interactions.inject_duration_ms, interactions.app_context, interactions.word_count";

/// Run a history query.
///
/// # Errors
///
/// Propagates SQLite failures.
pub fn query(conn: &Connection, q: &HistoryQuery) -> Result<HistoryPage> {
    // A store with history disabled has no `interactions` table at all, so the
    // query below would fail with "no such table". An empty page is the honest
    // answer for a user who turned history off, and it saves every caller from
    // special-casing a error that is not one.
    if !table_exists(conn, "interactions")? {
        return Ok(HistoryPage {
            items: Vec::new(),
            total: Some(0),
            next_offset: None,
        });
    }

    let mut where_clauses: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut from = "interactions";

    if let Some(text) = q.text.as_deref().filter(|text| !text.trim().is_empty()) {
        // Quoting each word makes this a literal user-facing search rather
        // than exposing FTS grammar through the CLI/API.
        from = "interactions JOIN interactions_fts ON interactions_fts.rowid = interactions.id";
        where_clauses.push("interactions_fts MATCH ?".into());
        binds.push(Box::new(fts_literal_query(text)));
    }
    if let Some(route) = &q.route {
        where_clauses.push("route_type = ?".into());
        binds.push(Box::new(route.as_str().to_string()));
    }
    if let Some(session) = &q.session_id {
        where_clauses.push("session_id = ?".into());
        binds.push(Box::new(session.as_str().to_string()));
    }
    // Timestamps are RFC3339 UTC strings, which sort lexicographically in the
    // same order they sort chronologically — so a string comparison is a
    // correct range filter here, not a coincidence worth relying on silently.
    if let Some(since) = q.since_ms {
        where_clauses.push("timestamp >= ?".into());
        binds.push(Box::new(ms_to_rfc3339(since)));
    }
    if let Some(until) = q.until_ms {
        where_clauses.push("timestamp <= ?".into());
        binds.push(Box::new(ms_to_rfc3339(until)));
    }
    if q.errors_only.unwrap_or(false) {
        where_clauses.push("error_summary IS NOT NULL".into());
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_clauses.join(" AND "))
    };

    let total: u64 = {
        let sql = format!("SELECT COUNT(*) FROM {from}{where_sql}");
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_row(rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())), |r| {
            r.get::<_, i64>(0)
        })? as u64
    };

    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = q.offset.unwrap_or(0);
    let order = match q.order {
        SortOrder::Ascending => "ASC",
        _ => "DESC",
    };

    let sql = format!(
        "SELECT {COLUMNS} FROM {from}{where_sql} ORDER BY interactions.id {order} LIMIT ? OFFSET ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    binds.push(Box::new(limit));
    binds.push(Box::new(offset));

    let rows = stmt.query_map(
        rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
        map_row,
    )?;
    let items: Vec<HistoryEntry> = rows.collect::<rusqlite::Result<_>>()?;

    let returned = items.len() as u32;
    let next_offset = (u64::from(offset) + u64::from(returned) < total)
        .then_some(offset.saturating_add(returned));

    Ok(HistoryPage {
        items,
        total: Some(total),
        next_offset,
    })
}

fn map_row(row: &Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let id: i64 = row.get(0)?;
    let session_id: String = row.get(1)?;
    let timestamp: String = row.get(2)?;
    let text: Option<String> = row.get(3)?;
    let raw_text: Option<String> = row.get(4)?;
    let route: Option<String> = row.get(5)?;
    let stt_s: Option<f64> = row.get(6)?;
    let fmt_s: Option<f64> = row.get(7)?;
    let fmt_error: Option<String> = row.get(8)?;
    let total_s: Option<f64> = row.get(9)?;
    let audio_s: Option<f64> = row.get(10)?;
    let error_summary: Option<String> = row.get(11)?;
    let capture_ms: Option<f64> = row.get(12)?;
    let vad_ms: Option<f64> = row.get(13)?;
    let stt_ms: Option<f64> = row.get(14)?;
    let fmt_rules_ms: Option<f64> = row.get(15)?;
    let fmt_llm_ms: Option<f64> = row.get(16)?;
    let inject_ms: Option<f64> = row.get(17)?;
    let app: Option<String> = row.get(18)?;
    let stored_word_count: Option<u32> = row.get(19)?;

    let timings = StageTimings {
        capture: timing(capture_ms),
        vad: timing(vad_ms),
        stt: timing(stt_ms.or_else(|| stt_s.map(|s| s * 1000.0))),
        fmt_rules: timing(fmt_rules_ms),
        // A recorded grammar error means the pass burned its time and then
        // fell back; that cost belongs in the budget, so it is `Failed`.
        fmt_llm: match (fmt_llm_ms.or_else(|| fmt_s.map(|s| s * 1000.0)), fmt_error) {
            (Some(ms), Some(e)) => StageTiming::Failed {
                ms,
                error: Some(e),
            },
            (Some(ms), None) => StageTiming::ran(ms),
            (None, _) => StageTiming::NotReported,
        },
        inject: timing(inject_ms),
        total_ms: total_s.map(|s| s * 1000.0),
        audio_ms: audio_s.map(|s| s * 1000.0),
    };

    let word_count = stored_word_count.or_else(|| text.as_ref().map(|t| t.split_whitespace().count() as u32));
    let wpm = match (&word_count, audio_s) {
        (Some(w), Some(secs)) if secs > 0.0 => Some(f64::from(*w) / (secs / 60.0)),
        _ => None,
    };

    Ok(HistoryEntry {
        id,
        session_id: SessionId(session_id),
        ts_ms: rfc3339_to_ms(&timestamp),
        text,
        raw_text,
        route: route.map_or(Route::Type, Route::from),
        timings,
        word_count,
        wpm,
        error: error_summary.map(|m| ProtoError::new(ErrorCode::Internal, m)),
        app,
    })
}

fn timing(ms: Option<f64>) -> StageTiming {
    ms.map_or(StageTiming::NotReported, StageTiming::ran)
}

fn fts_literal_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

fn ms_to_rfc3339(ms: u64) -> String {
    Utc.timestamp_millis_opt(ms as i64)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

fn rfc3339_to_ms(s: &str) -> u64 {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis().max(0) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{HistoryStore, Interaction};
    use crate::HistoryConfig;

    fn store_with(rows: Vec<Interaction>) -> HistoryStore {
        let dir = std::env::temp_dir().join(format!(
            "dictate-history-query-{}-{:p}",
            std::process::id(),
            &rows
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = HistoryStore::new(&HistoryConfig {
            enabled: true,
            db_path: dir.join("h.db").to_string_lossy().into_owned(),
            max_response_length: 10_000,
            ..Default::default()
        })
        .unwrap();
        for row in &rows {
            store.commit(row);
        }
        store
    }

    fn interaction(text: &str, route: &str) -> Interaction {
        Interaction {
            session_id: "sess".into(),
            timestamp: Utc::now().to_rfc3339(),
            corrected_transcription: Some(text.into()),
            raw_transcription: Some(text.into()),
            route_type: Some(route.into()),
            transcription_duration_s: Some(0.4),
            audio_duration_s: Some(2.0),
            completed: true,
            ..Interaction::default()
        }
    }

    #[test]
    fn absent_text_is_not_empty_text() {
        let mut silent = interaction("", "type");
        silent.corrected_transcription = Some(String::new());
        let mut private = interaction("secret", "type");
        private.corrected_transcription = None;
        private.raw_transcription = None;

        let store = store_with(vec![silent, private]);
        let page = query(store.connection(), &HistoryQuery::default()).unwrap();
        assert_eq!(page.items.len(), 2);

        let texts: Vec<Option<String>> = page.items.iter().map(|i| i.text.clone()).collect();
        assert!(
            texts.contains(&Some(String::new())),
            "a silent session must read as empty, not absent"
        );
        assert!(
            texts.contains(&None),
            "a withheld transcript must read as absent, not empty"
        );
    }

    #[test]
    fn unmeasured_stages_are_not_reported_rather_than_zero() {
        let store = store_with(vec![interaction("hello there", "type")]);
        let page = query(store.connection(), &HistoryQuery::default()).unwrap();
        let t = &page.items[0].timings;
        assert_eq!(t.capture, StageTiming::NotReported);
        assert_eq!(t.inject, StageTiming::NotReported);
        assert!(matches!(t.stt, StageTiming::Ran { .. }));
    }

    #[test]
    fn a_failed_format_pass_keeps_its_cost() {
        let mut row = interaction("hello there", "type");
        row.grammar_duration_s = Some(3.0);
        row.grammar_error = Some("ollama timed out".into());
        let store = store_with(vec![row]);
        let page = query(store.connection(), &HistoryQuery::default()).unwrap();
        match &page.items[0].timings.fmt_llm {
            StageTiming::Failed { ms, error } => {
                assert!((ms - 3000.0).abs() < 1.0);
                assert_eq!(error.as_deref(), Some("ollama timed out"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn text_filter_matches_a_substring() {
        let store = store_with(vec![
            interaction("hello there", "type"),
            interaction("goodbye now", "type"),
        ]);
        let page = query(
            store.connection(),
            &HistoryQuery {
                text: Some("hello".into()),
                ..HistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.total, Some(1));
    }

    #[test]
    fn route_filter_narrows_results() {
        let store = store_with(vec![
            interaction("hello there", "type"),
            interaction("five minutes", "timer"),
        ]);
        let page = query(
            store.connection(),
            &HistoryQuery {
                route: Some(Route::Timer),
                ..HistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].route, Route::Timer);
    }

    #[test]
    fn paging_reports_a_next_offset_only_while_more_remain() {
        let store = store_with((0..5).map(|i| interaction(&format!("row {i}"), "type")).collect());
        let page = query(
            store.connection(),
            &HistoryQuery {
                limit: Some(2),
                ..HistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.total, Some(5));
        assert_eq!(page.next_offset, Some(2));

        let last = query(
            store.connection(),
            &HistoryQuery {
                limit: Some(2),
                offset: Some(4),
                ..HistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(last.items.len(), 1);
        assert_eq!(last.next_offset, None, "no next page past the end");
    }

    #[test]
    fn errors_only_selects_failed_sessions() {
        let mut failed = interaction("boom", "type");
        failed.error_summary = Some("Transcription failed".into());
        let store = store_with(vec![interaction("fine", "type"), failed]);
        let page = query(
            store.connection(),
            &HistoryQuery {
                errors_only: Some(true),
                ..HistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].error.is_some());
    }

    #[test]
    fn a_limit_beyond_the_ceiling_is_clamped() {
        let store = store_with(vec![interaction("hello", "type")]);
        let page = query(
            store.connection(),
            &HistoryQuery {
                limit: Some(100_000),
                ..HistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);
    }

    #[test]
    fn a_quote_in_the_search_text_is_bound_not_interpolated() {
        let store = store_with(vec![interaction("hello there", "type")]);
        // Would be a syntax error or a dropped table if this were concatenated.
        let page = query(
            store.connection(),
            &HistoryQuery {
                text: Some("'; DROP TABLE interactions; --".into()),
                ..HistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(page.items.len(), 0);
        let all = query(store.connection(), &HistoryQuery::default()).unwrap();
        assert_eq!(all.items.len(), 1, "the table must still be there");
    }
}
