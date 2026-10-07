// Typed wrappers over the Tauri commands in src-tauri/src/commands.rs. Each
// maps 1:1 to a dictate-proto command; a rejection is a BridgeError.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  BridgeError,
  ConfigEntry,
  ConfigSnapshot,
  Connection,
  DaemonEvent,
  DiagnosticsReport,
  DictionaryEntry,
  DictionarySuggestion,
  HistoryAnalytics,
  HistoryPage,
  HistoryQuery,
  Status,
} from "./protocol";

export const daemon = {
  connectionState: () => invoke<Connection>("connection_state"),
  reconnect: () => invoke<void>("reconnect"),
  toggle: () => invoke<unknown>("toggle"),
  cancel: () => invoke<unknown>("cancel"),
  getStatus: () => invoke<Status>("get_status"),
  getConfig: (path?: string) => invoke<ConfigSnapshot>("get_config", { path: path ?? null }),
  setConfig: (args: { entries?: ConfigEntry[]; document?: string; dry_run?: boolean }) =>
    invoke<ConfigSnapshot>("set_config", {
      entries: args.entries ?? null,
      document: args.document ?? null,
      dry_run: args.dry_run ?? null,
    }),
  listDictionary: (query?: string) =>
    invoke<{ entries: DictionaryEntry[] }>("list_dictionary", { query: query || null, limit: 1000 }),
  listSuggestions: () =>
    invoke<{ suggestions: DictionarySuggestion[] }>("list_dictionary_suggestions", { limit: 50 }),
  upsertEntry: (entry: DictionaryEntry) =>
    invoke<{ entry: DictionaryEntry }>("upsert_dictionary_entry", { entry }),
  deleteEntry: (id: number) => invoke<unknown>("delete_dictionary_entry", { id }),
  queryHistory: (query: HistoryQuery) => invoke<HistoryPage>("query_history", { query }),
  analytics: () => invoke<HistoryAnalytics>("get_history_analytics"),
  diagnose: (quick: boolean) => invoke<DiagnosticsReport>("diagnose", { quick }),
  copyText: (text: string) => invoke<void>("copy_text", { text }),
  lastSessionEvent: () => invoke<DaemonEvent | null>("last_session_event"),
};

export function onDaemonEvent(handler: (event: DaemonEvent) => void): Promise<UnlistenFn> {
  return listen<DaemonEvent>("daemon-event", (e) => handler(e.payload));
}

export function onConnection(handler: (connection: Connection) => void): Promise<UnlistenFn> {
  return listen<Connection>("daemon-connection", (e) => handler(e.payload));
}

export function onNavigate(handler: (page: string) => void): Promise<UnlistenFn> {
  return listen<string>("hub-navigate", (e) => handler(e.payload));
}

/** A rejection from a daemon command, whatever shape it arrived in. */
export function asBridgeError(error: unknown): BridgeError {
  if (error && typeof error === "object" && "code" in error && "message" in error) {
    return error as BridgeError;
  }
  return { code: "internal", message: String(error) };
}
