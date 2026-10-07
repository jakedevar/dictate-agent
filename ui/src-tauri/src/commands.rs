//! Tauri commands. Each daemon-facing command maps 1:1 to a `dictate-proto`
//! [`Command`]: arguments are decoded into the protocol's own types at this
//! boundary (so a malformed entry is refused here, not by the daemon), and the
//! daemon's result is returned as the raw JSON it sent.
//!
//! Argument names are snake_case, matching the protocol, not Tauri's default
//! camelCase.

use std::time::Duration;

use dictate_proto::{
    Command, ConfigEntry, DictationMode, DictionaryEntry, HistoryQuery, SessionOptions,
};
use serde_json::Value;
use tauri::{AppHandle, State};

use crate::bridge::{Bridge, BridgeError, Connection};

type Reply = Result<Value, BridgeError>;

/// The bridge's connection state.
#[tauri::command]
pub fn connection_state(bridge: State<'_, Bridge>) -> Connection {
    bridge.connection()
}

/// Retry the daemon connection now instead of waiting out the backoff.
#[tauri::command]
pub fn reconnect(bridge: State<'_, Bridge>) {
    bridge.reconnect_now();
}

/// `toggle`
#[tauri::command]
pub async fn toggle(bridge: State<'_, Bridge>) -> Reply {
    bridge.request(Command::Toggle).await
}

/// `start_dictation`
#[tauri::command(rename_all = "snake_case")]
pub async fn start_dictation(
    bridge: State<'_, Bridge>,
    mode: Option<DictationMode>,
    options: Option<SessionOptions>,
) -> Reply {
    bridge
        .request(Command::StartDictation {
            mode: mode.unwrap_or_default(),
            options,
        })
        .await
}

/// `stop`
#[tauri::command]
pub async fn stop(bridge: State<'_, Bridge>) -> Reply {
    bridge.request(Command::Stop).await
}

/// `cancel`
#[tauri::command]
pub async fn cancel(bridge: State<'_, Bridge>) -> Reply {
    bridge.request(Command::Cancel).await
}

/// `get_status`
#[tauri::command]
pub async fn get_status(bridge: State<'_, Bridge>) -> Reply {
    bridge.request(Command::GetStatus).await
}

/// `get_config`
#[tauri::command(rename_all = "snake_case")]
pub async fn get_config(bridge: State<'_, Bridge>, path: Option<String>) -> Reply {
    bridge.request(Command::GetConfig { path }).await
}

/// `set_config`
#[tauri::command(rename_all = "snake_case")]
pub async fn set_config(
    bridge: State<'_, Bridge>,
    entries: Option<Vec<ConfigEntry>>,
    document: Option<String>,
    dry_run: Option<bool>,
    base_revision: Option<String>,
) -> Reply {
    bridge
        .request(Command::SetConfig {
            entries: entries.unwrap_or_default(),
            document,
            dry_run: dry_run.unwrap_or(false),
            base_revision,
        })
        .await
}

/// `list_dictionary`
#[tauri::command(rename_all = "snake_case")]
pub async fn list_dictionary(
    bridge: State<'_, Bridge>,
    query: Option<String>,
    limit: Option<u32>,
) -> Reply {
    bridge
        .request(Command::ListDictionary { query, limit })
        .await
}

/// `list_dictionary_suggestions`
#[tauri::command(rename_all = "snake_case")]
pub async fn list_dictionary_suggestions(bridge: State<'_, Bridge>, limit: Option<u32>) -> Reply {
    bridge
        .request(Command::ListDictionarySuggestions { limit })
        .await
}

/// `upsert_dictionary_entry`
#[tauri::command(rename_all = "snake_case")]
pub async fn upsert_dictionary_entry(bridge: State<'_, Bridge>, entry: DictionaryEntry) -> Reply {
    bridge
        .request(Command::UpsertDictionaryEntry { entry })
        .await
}

/// `delete_dictionary_entry`
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_dictionary_entry(bridge: State<'_, Bridge>, id: i64) -> Reply {
    bridge.request(Command::DeleteDictionaryEntry { id }).await
}

/// `list_notes` (S35)
#[tauri::command(rename_all = "snake_case")]
pub async fn list_notes(
    bridge: State<'_, Bridge>,
    query: Option<String>,
    limit: Option<u32>,
) -> Reply {
    bridge
        .request(Command::ListNotes {
            query,
            limit,
            id: None,
        })
        .await
}

/// `delete_note` (S35)
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_note(bridge: State<'_, Bridge>, id: i64) -> Reply {
    bridge.request(Command::DeleteNote { id }).await
}

/// `query_history`
#[tauri::command(rename_all = "snake_case")]
pub async fn query_history(bridge: State<'_, Bridge>, query: Option<HistoryQuery>) -> Reply {
    bridge
        .request(Command::QueryHistory {
            query: query.unwrap_or_default(),
        })
        .await
}

/// `get_history_analytics`
#[tauri::command]
pub async fn get_history_analytics(bridge: State<'_, Bridge>) -> Reply {
    bridge.request(Command::GetHistoryAnalytics).await
}

/// `purge_history`
#[tauri::command]
pub async fn purge_history(bridge: State<'_, Bridge>) -> Reply {
    bridge.request(Command::PurgeHistory).await
}

/// `diagnose`. A full run hashes the model file, so it gets a longer deadline.
#[tauri::command(rename_all = "snake_case")]
pub async fn diagnose(bridge: State<'_, Bridge>, quick: Option<bool>) -> Reply {
    let quick = quick.unwrap_or(true);
    let deadline = if quick { 30 } else { 180 };
    bridge
        .request_within(Command::Diagnose { quick }, Duration::from_secs(deadline))
        .await
}

/// Put text on the clipboard (the History page's copy button). Done natively
/// because WebKitGTK's async clipboard API is unreliable on a custom scheme.
#[tauri::command]
pub fn copy_text(app: AppHandle, text: String) -> Result<(), String> {
    crate::app::copy_to_clipboard(&app, text)
}

/// The most recent `state_changed` event, so a webview that loaded after the
/// session began can catch up.
#[tauri::command]
pub fn last_session_event(sink: State<'_, std::sync::Arc<crate::app::AppSink>>) -> Option<Value> {
    sink.last_session_event()
}

/// Bring up the hub, optionally on a page.
#[tauri::command]
pub fn open_hub(app: AppHandle, page: Option<String>) -> Result<(), String> {
    crate::app::open_hub(&app, page.as_deref()).map_err(|e| e.to_string())
}
