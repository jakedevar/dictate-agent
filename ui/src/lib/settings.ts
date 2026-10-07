// The structured settings editor's model: which keys it shows, how their
// inputs parse, and which entries a save sends. Everything else lives in the
// raw TOML editor, which covers every key the daemon reads.

import type { ConfigEntry } from "./protocol";

export type FieldKind = "bool" | "text" | "number" | "optional-number" | "select";

export interface FieldSpec {
  path: string;
  label: string;
  help: string;
  kind: FieldKind;
  options?: readonly string[];
  min?: number;
  step?: number;
}

export interface SectionSpec {
  title: string;
  fields: readonly FieldSpec[];
}

/** Keys the daemon applies without a restart (dictated/src/config_rpc.rs LIVE_KEYS). */
export const LIVE_KEYS: ReadonlySet<string> = new Set(["history.privacy_mode"]);

export const SECTIONS: readonly SectionSpec[] = [
  {
    title: "Recognition",
    fields: [
      { path: "whisper.model", label: "Speech model", help: "whisper.cpp catalog name, e.g. large-v3-turbo.", kind: "text" },
      { path: "whisper.language", label: "Language", help: "BCP-47 code, or auto to detect.", kind: "text" },
      { path: "whisper.device", label: "Device", help: "cuda for the GPU, cpu as a fallback.", kind: "select", options: ["cuda", "cpu"] },
    ],
  },
  {
    title: "Formatting",
    fields: [
      { path: "grammar.enabled", label: "LLM polish", help: "Run the local LLM pass over prose. Fails open to the rules output.", kind: "bool" },
      { path: "grammar.model", label: "Formatter model", help: "An Ollama model that is installed (see Doctor).", kind: "text" },
      { path: "grammar.min_words", label: "Minimum words", help: "Shorter dictations skip the LLM pass.", kind: "number", min: 0, step: 1 },
      { path: "grammar.timeout_s", label: "Timeout (s)", help: "Give up on the LLM after this long.", kind: "number", min: 0.1, step: 0.5 },
    ],
  },
  {
    title: "Audio",
    fields: [
      { path: "audio.capture", label: "Microphone capture", help: "Off makes the daemon audio-less (uploads only).", kind: "bool" },
      { path: "audio.input_device", label: "Input device", help: "Exact device name; empty uses the default.", kind: "text" },
      { path: "audio.pre_roll_ms", label: "Pre-roll (ms)", help: "Audio kept from just before you start. 0 releases the microphone while idle.", kind: "number", min: 0, step: 50 },
      { path: "audio.earcons.enabled", label: "Start/stop sounds", help: "Short chimes on start, stop and cancel.", kind: "bool" },
      { path: "vad.enabled", label: "Trim silence", help: "Voice activity detection before transcription.", kind: "bool" },
    ],
  },
  {
    title: "Output",
    fields: [
      { path: "output.auto_type", label: "Insert text", help: "Paste the result into the focused window.", kind: "bool" },
      { path: "output.policy", label: "Insertion method", help: "paste (clipboard), type (keystrokes) or off.", kind: "select", options: ["paste", "type", "off"] },
      { path: "notifications.enabled", label: "Desktop notifications", help: "", kind: "bool" },
    ],
  },
  {
    title: "Privacy & history",
    fields: [
      { path: "history.enabled", label: "Keep history", help: "Record dictations locally for search and analytics.", kind: "bool" },
      { path: "history.privacy_mode", label: "Privacy mode", help: "Store nothing about new dictations. Applies immediately.", kind: "bool" },
      { path: "history.retention_days", label: "Keep for (days)", help: "Empty keeps everything.", kind: "optional-number", min: 1, step: 1 },
    ],
  },
  {
    title: "Dictionary",
    fields: [
      { path: "dictionary.enabled", label: "Personal dictionary", help: "Apply your terms and bias recognition toward them.", kind: "bool" },
      { path: "dictionary.fuzzy", label: "Fuzzy matching", help: "Also replace near-misses. Off by default: it can change words you meant.", kind: "bool" },
    ],
  },
];

export function getPath(values: unknown, path: string): unknown {
  let node: unknown = values;
  for (const seg of path.split(".")) {
    if (node === null || typeof node !== "object" || !(seg in node)) return undefined;
    node = (node as Record<string, unknown>)[seg];
  }
  return node;
}

export function hasPath(values: unknown, path: string): boolean {
  return getPath(values, path) !== undefined;
}

/** Sections with only the fields this daemon actually has. */
export function visibleSections(values: unknown): SectionSpec[] {
  return SECTIONS.map((s) => ({ ...s, fields: s.fields.filter((f) => hasPath(values, f.path)) })).filter(
    (s) => s.fields.length > 0,
  );
}

export type Parsed = { ok: true; value: unknown } | { ok: false; error: string };

/** Turn what an input holds into the JSON value to send. */
export function parseInput(spec: FieldSpec, raw: string | boolean): Parsed {
  switch (spec.kind) {
    case "bool":
      return { ok: true, value: raw === true || raw === "true" };
    case "text":
      return { ok: true, value: String(raw) };
    case "select": {
      const v = String(raw);
      return spec.options && !spec.options.includes(v)
        ? { ok: false, error: `choose one of ${spec.options.join(", ")}` }
        : { ok: true, value: v };
    }
    case "number":
    case "optional-number": {
      const text = String(raw).trim();
      if (text === "") {
        return spec.kind === "optional-number" ? { ok: true, value: null } : { ok: false, error: "required" };
      }
      const n = Number(text);
      if (!Number.isFinite(n)) return { ok: false, error: "not a number" };
      if (spec.min !== undefined && n < spec.min) return { ok: false, error: `at least ${spec.min}` };
      if (spec.step !== undefined && Number.isInteger(spec.step) && !Number.isInteger(n)) {
        return { ok: false, error: "a whole number" };
      }
      return { ok: true, value: n };
    }
  }
}

/**
 * The entries a save should send: only fields whose value differs from what
 * the daemon reported, so an untouched form writes nothing to the file.
 */
export function changedEntries(values: unknown, edits: ReadonlyMap<string, unknown>): ConfigEntry[] {
  const out: ConfigEntry[] = [];
  for (const [path, value] of edits) {
    const current = getPath(values, path);
    const same = (current ?? null) === (value ?? null);
    if (!same) out.push({ path, value });
  }
  return out;
}

/** The value an input should display. */
export function displayValue(spec: FieldSpec, value: unknown): string | boolean {
  if (spec.kind === "bool") return value === true;
  if (value === null || value === undefined) return "";
  return String(value);
}
