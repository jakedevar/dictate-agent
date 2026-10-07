// The structured settings editor's model: which keys it shows, how their
// inputs parse, and which entries a save sends. Everything else lives in the
// raw TOML editor, which covers every key the daemon reads.

import { splitList } from "./format";
import type { ConfigEntry } from "./protocol";

export type FieldKind = "bool" | "text" | "number" | "optional-number" | "select" | "list";

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
      { path: "format.enabled", label: "Cleanup rules", help: "Deterministic cleanup: fillers, stutters, casing, spacing.", kind: "bool" },
      { path: "format.rules.spoken_punctuation", label: "Spoken punctuation", help: "“comma”, “period” … become punctuation. Off by default: code prompts use these words literally.", kind: "bool" },
      { path: "format.rules.spoken_line_breaks", label: "Spoken line breaks", help: "“new line” and “new paragraph” become line breaks.", kind: "bool" },
      { path: "format.llm.enabled", label: "LLM polish", help: "Run the local LLM pass over prose. Fails open to the rules output.", kind: "bool" },
      { path: "format.llm.models", label: "Formatter models", help: "Ollama models in order of preference, comma-separated. The first installed one is used (see Doctor).", kind: "list" },
      { path: "format.llm.min_words", label: "Minimum words", help: "Shorter dictations skip the LLM pass.", kind: "number", min: 0, step: 1 },
      { path: "format.llm.keep_alive", label: "Keep model loaded", help: "Ollama keep_alive, e.g. 30m, 1h, or -1 for always.", kind: "text" },
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

/**
 * The formatter fields for a file that still configures the LLM pass through
 * the deprecated `[grammar]` section. The daemon reads `[grammar]` only while
 * `[format.llm]` is absent, so writing `format.llm.*` into such a file would
 * create that section and silently discard the user's `[grammar]` settings.
 * For those files the editor edits `[grammar]` instead.
 */
export const LEGACY_FORMATTER: SectionSpec = {
  title: "Formatting",
  fields: [
    { path: "format.enabled", label: "Cleanup rules", help: "Deterministic cleanup: fillers, stutters, casing, spacing.", kind: "bool" },
    { path: "grammar.enabled", label: "LLM polish", help: "Run the local LLM pass over prose. Fails open to the rules output.", kind: "bool" },
    { path: "grammar.model", label: "Formatter model", help: "Tried first; if it is not installed the default ladder is used (see Doctor).", kind: "text" },
    { path: "grammar.min_words", label: "Minimum words", help: "Shorter dictations skip the LLM pass.", kind: "number", min: 0, step: 1 },
    { path: "grammar.timeout_s", label: "Timeout (s)", help: "Give up on the LLM after this long.", kind: "number", min: 0.1, step: 0.5 },
  ],
};

/** Every field either layout can show. */
export const ALL_FIELDS: readonly FieldSpec[] = [...SECTIONS.flatMap((s) => s.fields), ...LEGACY_FORMATTER.fields];

/**
 * Whether the file configures the LLM pass only through the legacy
 * `[grammar]` section (no `[format.llm]` anywhere).
 *
 * Reads the document's *key paths* the way TOML defines them, so every spelling
 * of the same table counts: `[grammar]`, `[ "grammar" ]`, `['grammar']`,
 * `[[grammar]]`, a top-level `grammar.enabled = true`, an inline
 * `grammar = { enabled = true }`, and likewise for `format.llm` (headers,
 * dotted keys inside `[format]`, nested inline tables). Comments and string
 * contents never match. It is a layout choice only: the daemon validates every
 * write regardless.
 */
export function usesLegacyGrammar(document: string): boolean {
  const paths = tomlKeyPaths(document);
  const has = (prefix: readonly string[]) =>
    paths.some((p) => p.length >= prefix.length && prefix.every((seg, i) => p[i] === seg));
  return has(["grammar"]) && !has(["format", "llm"]);
}

/**
 * Every key path a TOML document defines: table headers, dotted and nested
 * keys, and the keys inside inline tables (values themselves are skipped). A
 * document this scanner cannot follow yields the paths found before the
 * problem, and the daemon is the judge of validity.
 */
export function tomlKeyPaths(document: string): string[][] {
  const out: string[][] = [];
  const src = document;
  let i = 0;

  const peek = (n = 0) => src[i + n];
  const skipInline = () => {
    while (i < src.length && (peek() === " " || peek() === "\t")) i++;
  };
  const skipBlank = () => {
    for (;;) {
      const c = peek();
      if (c === " " || c === "\t" || c === "\n" || c === "\r") i++;
      else if (c === "#") while (i < src.length && peek() !== "\n") i++;
      else return;
    }
  };
  const fail = (): never => {
    throw new Error("toml");
  };

  const quoted = (): string => {
    const q = peek();
    i++;
    let text = "";
    while (i < src.length && peek() !== q) {
      if (peek() === "\n") fail();
      if (q === '"' && peek() === "\\") {
        const next = peek(1);
        const map: Record<string, string> = { n: "\n", t: "\t", r: "\r", b: "\b", f: "\f", '"': '"', "\\": "\\" };
        if (next === "u" || next === "U") {
          const len = next === "u" ? 4 : 8;
          const code = Number.parseInt(src.slice(i + 2, i + 2 + len), 16);
          text += Number.isNaN(code) ? "" : String.fromCodePoint(code);
          i += 2 + len;
          continue;
        }
        text += map[next ?? ""] ?? next ?? "";
        i += 2;
        continue;
      }
      text += peek();
      i++;
    }
    if (peek() !== q) fail();
    i++;
    return text;
  };

  const keyPart = (): string => {
    skipInline();
    const c = peek();
    if (c === '"' || c === "'") return quoted();
    const m = /^[A-Za-z0-9_-]+/.exec(src.slice(i, i + 256));
    if (!m) return fail();
    i += m[0].length;
    return m[0];
  };

  const dottedKey = (): string[] => {
    const parts = [keyPart()];
    for (;;) {
      skipInline();
      if (peek() !== ".") return parts;
      i++;
      parts.push(keyPart());
    }
  };

  const multiline = (q: string) => {
    i += 3;
    for (;;) {
      if (i >= src.length) fail();
      if (q === '"' && peek() === "\\") {
        i += 2;
        continue;
      }
      if (src.startsWith(q.repeat(3), i)) {
        i += 3;
        // Up to two extra quote characters may close the string.
        while (peek() === q && i < src.length) i++;
        return;
      }
      i++;
    }
  };

  const value = (prefix: string[]) => {
    skipInline();
    const c = peek();
    if (c === '"' || c === "'") {
      if (src.startsWith(c.repeat(3), i)) multiline(c);
      else quoted();
    } else if (c === "{") {
      i++;
      skipBlank();
      if (peek() === "}") {
        i++;
        return;
      }
      for (;;) {
        skipBlank();
        const key = [...prefix, ...dottedKey()];
        out.push(key);
        skipInline();
        if (peek() !== "=") fail();
        i++;
        value(key);
        skipBlank();
        if (peek() === ",") {
          i++;
          continue;
        }
        if (peek() === "}") {
          i++;
          return;
        }
        fail();
      }
    } else if (c === "[") {
      i++;
      for (;;) {
        skipBlank();
        if (peek() === "]") {
          i++;
          return;
        }
        value(prefix);
        skipBlank();
        if (peek() === ",") i++;
        else if (peek() !== "]") fail();
      }
    } else {
      while (i < src.length && !",]}\n#".includes(peek() as string)) i++;
    }
  };

  try {
    let table: string[] = [];
    for (;;) {
      skipBlank();
      if (i >= src.length) break;
      if (peek() === "[") {
        const array = peek(1) === "[";
        i += array ? 2 : 1;
        table = dottedKey();
        out.push(table);
        skipInline();
        if (peek() !== "]") fail();
        i++;
        if (array) {
          skipInline();
          if (peek() !== "]") fail();
          i++;
        }
      } else {
        const key = [...table, ...dottedKey()];
        out.push(key);
        skipInline();
        if (peek() !== "=") fail();
        i++;
        value(key);
      }
      skipInline();
      if (peek() === "#") while (i < src.length && peek() !== "\n") i++;
      else if (i < src.length && peek() !== "\n" && peek() !== "\r") fail();
    }
  } catch {
    // Stop at the first thing this scanner does not follow.
  }
  return out;
}

/** The sections to show for this daemon and this file. */
export function sectionsFor(values: unknown, document: string): SectionSpec[] {
  const legacy = usesLegacyGrammar(document);
  const chosen = SECTIONS.map((s) => (legacy && s.title === "Formatting" ? LEGACY_FORMATTER : s));
  return visibleSections(values, chosen);
}

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
export function visibleSections(values: unknown, sections: readonly SectionSpec[] = SECTIONS): SectionSpec[] {
  return sections.map((s) => ({ ...s, fields: s.fields.filter((f) => hasPath(values, f.path)) })).filter(
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
    case "list": {
      const items = splitList(String(raw));
      return items.length === 0 ? { ok: false, error: "at least one entry" } : { ok: true, value: items };
    }
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
    const same = JSON.stringify(current ?? null) === JSON.stringify(value ?? null);
    if (!same) out.push({ path, value });
  }
  return out;
}

/** The value an input should display. */
export function displayValue(spec: FieldSpec, value: unknown): string | boolean {
  if (spec.kind === "bool") return value === true;
  if (value === null || value === undefined) return "";
  if (spec.kind === "list" && Array.isArray(value)) return value.map(String).join(", ");
  return String(value);
}
