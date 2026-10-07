import { useEffect, useState } from "preact/hooks";
import { asBridgeError, daemon, onDaemonEvent } from "../../lib/daemon";
import { relativeTime } from "../../lib/format";
import { emptyMessage, newestFirst, nextPhase, without, type Phase } from "../../lib/notes";
import type { Note, Status } from "../../lib/protocol";
import { useHub, useLoad } from "../useDaemon";

export function Notes() {
  const { refresh, connection } = useHub();
  const live = connection.status === "connected";
  const [search, setSearch] = useState("");
  const [query, setQuery] = useState("");
  const [notes, setNotes] = useState<Note[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<number | null>(null);
  const [confirming, setConfirming] = useState<number | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const status = useLoad<Status | null>(() => (live ? daemon.getStatus() : Promise.resolve(null)), [refresh, live]);

  useEffect(() => {
    const t = window.setTimeout(() => setQuery(search.trim()), 250);
    return () => window.clearTimeout(t);
  }, [search]);

  useEffect(() => {
    if (!live) return;
    let cancelled = false;
    daemon
      .listNotes(query)
      .then((r) => {
        if (cancelled) return;
        setNotes(newestFirst(r.notes));
        setError(null);
      })
      .catch((e) => !cancelled && setError(asBridgeError(e).message));
    return () => {
      cancelled = true;
    };
  }, [query, refresh, live]);

  // Follow the session so the button says what it will do.
  useEffect(() => {
    void daemon.lastSessionEvent().then((e) => setPhase((p) => nextPhase(p, e))).catch(() => {});
    const off = onDaemonEvent((e) => setPhase((p) => nextPhase(p, e)));
    return () => void off.then((f) => f());
  }, []);

  const privacy = status.data?.capabilities.features.privacy_mode === true;
  const now = Date.now();

  const copy = async (note: Note) => {
    try {
      await daemon.copyText(note.text);
      setCopied(note.id);
      window.setTimeout(() => setCopied((c) => (c === note.id ? null : c)), 1500);
    } catch (e) {
      setError(asBridgeError(e).message);
    }
  };

  const remove = async (note: Note) => {
    if (confirming !== note.id) {
      setConfirming(note.id);
      window.setTimeout(() => setConfirming((c) => (c === note.id ? null : c)), 4000);
      return;
    }
    try {
      await daemon.deleteNote(note.id);
      setNotes((prev) => without(prev, note.id));
      setConfirming(null);
    } catch (e) {
      setError(asBridgeError(e).message);
    }
  };

  const dictate = async () => {
    try {
      if (phase === "recording") await daemon.stop();
      else await daemon.startNote();
      setError(null);
    } catch (e) {
      setError(asBridgeError(e).message);
    }
  };

  return (
    <div class="page">
      <header class="page-head">
        <h1>Notes</h1>
        <span class={`badge ${privacy ? "badge-private" : ""}`} title="history.privacy_mode">
          {privacy ? "Privacy mode on: new notes are not saved" : "Notes are saved locally"}
        </span>
      </header>
      <div class="toolbar">
        <label class="search">
          <span class="visually-hidden">Search notes</span>
          <input
            type="search"
            placeholder="Search notes…"
            value={search}
            onInput={(e) => setSearch((e.target as HTMLInputElement).value)}
          />
        </label>
        <button type="button" onClick={() => void dictate()} disabled={!live || phase === "working"}>
          {phase === "recording" ? "Stop and save" : phase === "working" ? "Saving…" : "Dictate a note"}
        </button>
      </div>
      {error && <p class="error">{error}</p>}
      {notes.length === 0 && !error ? (
        <p class="muted">{emptyMessage(query)}</p>
      ) : (
        <ul class="history-list">
          {notes.map((n) => (
            <li key={n.id} class="history-item stored">
              <div class="meta">
                <time dateTime={new Date(n.ts_ms).toISOString()} title={new Date(n.ts_ms).toLocaleString()}>
                  {relativeTime(n.ts_ms, now)}
                </time>
                <span class="muted">{n.word_count} words</span>
              </div>
              <div class="body">
                <p class="text">{n.text}</p>
              </div>
              <div class="actions">
                <button type="button" class="ghost" onClick={() => void copy(n)} aria-label="Copy this note">
                  {copied === n.id ? "Copied" : "Copy"}
                </button>
                <button
                  type="button"
                  class="ghost danger"
                  onClick={() => void remove(n)}
                  aria-label={confirming === n.id ? "Confirm deleting this note" : "Delete this note"}
                >
                  {confirming === n.id ? "Confirm delete" : "Delete"}
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
