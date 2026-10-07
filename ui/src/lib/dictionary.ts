// The Dictionary page's model: form <-> entry, and what accepting or
// dismissing a suggestion writes. Pure, so it is unit-tested.

import { splitList } from "./format";
import type { DictionaryEntry, DictionarySuggestion } from "./protocol";

/** What the add/edit form holds. Lists are comma-separated text. */
export interface EntryForm {
  id?: number;
  phrase: string;
  soundsLike: string;
  apps: string;
  caseSensitive: boolean;
  enabled: boolean;
}

export function emptyForm(): EntryForm {
  return { phrase: "", soundsLike: "", apps: "", caseSensitive: false, enabled: true };
}

export function formFromEntry(entry: DictionaryEntry): EntryForm {
  return {
    ...(entry.id !== undefined ? { id: entry.id } : {}),
    phrase: entry.phrase,
    soundsLike: (entry.sounds_like ?? []).join(", "),
    apps: (entry.apps ?? []).join(", "),
    caseSensitive: entry.case_sensitive === true,
    enabled: entry.enabled !== false,
  };
}

export type FormResult = { ok: true; entry: DictionaryEntry } | { ok: false; field: "phrase" | "soundsLike"; error: string };

/**
 * The entry a form saves, or why it cannot. The daemon validates again; this
 * only catches what a person can fix before sending.
 */
export function entryFromForm(form: EntryForm, existing?: DictionaryEntry): FormResult {
  const phrase = form.phrase.trim();
  if (!phrase) return { ok: false, field: "phrase", error: "Enter the word or phrase as it should be written." };
  const soundsLike = splitList(form.soundsLike);
  if (soundsLike.some((s) => s.toLowerCase() === phrase.toLowerCase())) {
    return { ok: false, field: "soundsLike", error: "A spoken form identical to the phrase does nothing; remove it." };
  }
  const entry: DictionaryEntry = {
    phrase,
    sounds_like: soundsLike,
    apps: splitList(form.apps),
    case_sensitive: form.caseSensitive,
    enabled: form.enabled,
  };
  if (form.id !== undefined) entry.id = form.id;
  // Editing keeps where an entry came from (a learned entry stays learned).
  if (existing?.source !== undefined) entry.source = existing.source;
  return { ok: true, entry };
}

/** Accepting a suggestion adds it as an enabled entry. */
export function accepted(s: DictionarySuggestion): DictionaryEntry {
  return withoutId({ ...s.entry, enabled: true });
}

/**
 * Dismissing a suggestion stores it as a *disabled* entry. The daemon never
 * suggests a phrase the dictionary already holds, so this is what makes a
 * dismissal stick across restarts; a disabled entry changes no transcript and
 * can be enabled or deleted later from the list.
 */
export function dismissed(s: DictionarySuggestion): DictionaryEntry {
  return withoutId({ ...s.entry, enabled: false });
}

function withoutId(entry: DictionaryEntry): DictionaryEntry {
  const { id: _id, hit_count: _hits, ...rest } = entry;
  return rest;
}

export function describeReason(reason: string): string {
  switch (reason) {
    case "consistent_rewrite":
      return "You keep correcting it to this";
    case "recurring_term":
      return "A term you use often";
    default:
      return reason.replace(/_/g, " ");
  }
}

/** Entries sorted for display: enabled first, then alphabetically. */
export function sortEntries(entries: readonly DictionaryEntry[]): DictionaryEntry[] {
  return [...entries].sort((a, b) => {
    const ea = a.enabled !== false ? 0 : 1;
    const eb = b.enabled !== false ? 0 : 1;
    return ea - eb || a.phrase.localeCompare(b.phrase, undefined, { sensitivity: "base" });
  });
}
