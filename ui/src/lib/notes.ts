// Presentation model for the Notes page (S35). Pure; unit-tested.

import type { DaemonEvent, Note } from "./protocol";

/** Newest first, ties broken by id — what the daemon sends, kept stable client-side. */
export function newestFirst(notes: Note[]): Note[] {
  return [...notes].sort((a, b) => b.ts_ms - a.ts_ms || b.id - a.id);
}

/** The list without one note, for an optimistic delete. */
export function without(notes: Note[], id: number): Note[] {
  return notes.filter((n) => n.id !== id);
}

export type Phase = "idle" | "recording" | "working";

/**
 * Where a dictation session is, from the daemon's state events. Only the
 * states that matter to the Dictate-a-note button are distinguished: anything
 * between recording and a terminal state is "working" (the button is busy), a
 * terminal state or idle is "idle".
 */
export function nextPhase(prev: Phase, event: DaemonEvent | null): Phase {
  if (!event || event.type !== "state_changed") return prev;
  const to = String((event as { to?: unknown }).to);
  if (to === "recording") return "recording";
  if (["done", "error", "cancelled", "idle"].includes(to)) return "idle";
  return "working";
}

/** The empty-state sentence, which doubles as the instructions for the feature. */
export function emptyMessage(query: string): string {
  return query
    ? `No notes match “${query}”.`
    : "No notes yet. Say “note: …” or “note to self …” while dictating, or press Dictate a note.";
}
