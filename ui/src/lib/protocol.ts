// The subset of dictate-proto the UI reads. The Rust crate
// (crates/dictate-proto) and docs/protocol.md are the source of truth; these
// types only describe what this UI touches, and every field the daemon may
// omit is optional here too. Unknown event types and unknown fields are
// tolerated everywhere (the protocol's compatibility rule).

export type SessionState =
  | "idle"
  | "recording"
  | "transcribing"
  | "formatting"
  | "injecting"
  | "done"
  | "error"
  | "cancelled";

export interface ProtoError {
  code: string;
  message: string;
  detail?: unknown;
}

export type DaemonEvent =
  | { type: "state_changed"; session_id: string; from: string; to: string; at_ms?: number }
  | { type: "audio_level"; session_id: string; rms: number; peak?: number }
  | {
      type: "final";
      session_id: string;
      text: string;
      route?: string;
      word_count?: number;
      injection?: string;
    }
  | { type: "error"; session_id?: string; error: ProtoError }
  | { type: string; [key: string]: unknown };

export type Connection =
  | { status: "connecting"; attempt: number }
  | {
      status: "connected";
      server: string;
      version: string;
      protocol_version: number;
      features: Features;
    }
  | { status: "disconnected"; reason: string; retry_in_ms: number; attempt: number };

export interface Features {
  host_capture?: boolean;
  history_read?: boolean;
  history_write?: boolean;
  dictionary_read?: boolean;
  dictionary_write?: boolean;
  config_read?: boolean;
  config_write?: boolean;
  diagnostics?: boolean;
  privacy_mode?: boolean;
  headless?: boolean;
  text_injection?: boolean;
  audio_level_events?: boolean;
}

/** The rejection value of every daemon-facing Tauri command. */
export interface BridgeError {
  code: string;
  message: string;
  detail?: { path?: string | null; errors?: string[] } & Record<string, unknown>;
}

export interface FormatterStatus {
  enabled: boolean;
  model?: string;
  health: "disabled" | "unchecked" | "ok" | "model_missing" | "unreachable" | "failing" | string;
  detail?: string;
}

export interface Status {
  type: "status";
  state: SessionState | string;
  daemon: { name: string; version: string; protocol_version: number; pid?: number; uptime_ms?: number };
  model?: { name: string; loaded: boolean; backend?: string };
  capabilities: { features: Features };
  formatter?: FormatterStatus;
  audio?: { capture_enabled: boolean; input_open: boolean; pre_roll_ms?: number };
}

export interface DailyWords {
  day: string;
  words: number;
}

export interface HistoryAnalytics {
  type: "history_analytics";
  overall_wpm?: number;
  words_today: number;
  words_by_day: DailyWords[];
  current_streak_days: number;
  longest_streak_days: number;
}

export interface HistoryEntry {
  id: number;
  session_id: string;
  ts_ms: number;
  text?: string;
  raw_text?: string;
  route?: string;
  word_count?: number;
  wpm?: number;
  error?: ProtoError;
  app?: string;
}

/** A scratchpad note (S35). Mirrors `dictate_proto::Note`. */
export interface Note {
  id: number;
  ts_ms: number;
  text: string;
  word_count: number;
}

export interface NotesResult {
  type: "notes";
  notes: Note[];
}

export interface HistoryPage {
  type: "history";
  items: HistoryEntry[];
  total?: number;
  next_offset?: number;
}

export interface HistoryQuery {
  text?: string;
  limit?: number;
  offset?: number;
  errors_only?: boolean;
}

export interface DictionaryEntry {
  id?: number;
  phrase: string;
  sounds_like?: string[];
  apps?: string[];
  case_sensitive?: boolean;
  enabled?: boolean;
  source?: string;
  hit_count?: number;
}

export interface DictionarySuggestion {
  entry: DictionaryEntry;
  reason: string;
  count: number;
  days: number;
  first_seen: string;
  last_seen: string;
}

export interface ConfigEntry {
  path: string;
  value: unknown;
}

export interface ConfigSnapshot {
  type: "config";
  values: Record<string, unknown>;
  path?: string;
  applied?: string[];
  restart_required?: string[];
  file?: { path: string; exists: boolean; document: string; revision?: string };
  warnings?: string[];
  errors?: string[];
  dry_run?: boolean;
}

export interface DiagnosticCheck {
  id: string;
  title: string;
  status: "ok" | "warn" | "fail" | "skipped" | string;
  detail: string;
  fix?: string;
}

export interface DiagnosticsReport {
  type: "diagnostics";
  checks: DiagnosticCheck[];
}
