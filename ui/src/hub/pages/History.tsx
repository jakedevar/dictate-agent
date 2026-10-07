import { useEffect, useState } from "preact/hooks";
import { asBridgeError, daemon } from "../../lib/daemon";
import { formatWpm, relativeTime, storedState } from "../../lib/format";
import type { HistoryEntry, Status } from "../../lib/protocol";
import { useHub, useLoad } from "../useDaemon";

const PAGE = 50;

export function History() {
  const { refresh, connection } = useHub();
  const live = connection.status === "connected";
  const [search, setSearch] = useState("");
  const [query, setQuery] = useState("");
  const [items, setItems] = useState<HistoryEntry[]>([]);
  const [next, setNext] = useState<number | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<number | null>(null);
  const status = useLoad<Status | null>(() => (live ? daemon.getStatus() : Promise.resolve(null)), [refresh, live]);

  // Debounce typing into a search.
  useEffect(() => {
    const t = window.setTimeout(() => setQuery(search.trim()), 250);
    return () => window.clearTimeout(t);
  }, [search]);

  const load = async (offset: number) => {
    try {
      const page = await daemon.queryHistory({ ...(query ? { text: query } : {}), limit: PAGE, offset });
      setItems((prev) => (offset === 0 ? page.items : [...prev, ...page.items]));
      setNext(page.next_offset);
      setError(null);
    } catch (e) {
      setError(asBridgeError(e).message);
    }
  };

  useEffect(() => {
    if (live) void load(0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query, refresh, live]);

  const privacy = status.data?.capabilities.features.privacy_mode === true;
  const now = Date.now();

  const copy = async (entry: HistoryEntry) => {
    if (entry.text === undefined) return;
    try {
      await daemon.copyText(entry.text);
      setCopied(entry.id);
      window.setTimeout(() => setCopied((c) => (c === entry.id ? null : c)), 1500);
    } catch (e) {
      setError(asBridgeError(e).message);
    }
  };

  return (
    <div class="page">
      <header class="page-head">
        <h1>History</h1>
        <span class={`badge ${privacy ? "badge-private" : ""}`} title="history.privacy_mode">
          {privacy ? "Privacy mode on: new dictations are not stored" : "Recording history"}
        </span>
      </header>
      <div class="toolbar">
        <label class="search">
          <span class="visually-hidden">Search dictations</span>
          <input
            type="search"
            placeholder="Search dictations…"
            value={search}
            onInput={(e) => setSearch((e.target as HTMLInputElement).value)}
          />
        </label>
      </div>
      {error && <p class="error">{error}</p>}
      {items.length === 0 && !error ? (
        <p class="muted">{query ? `Nothing matches “${query}”.` : "No dictations recorded yet."}</p>
      ) : (
        <ul class="history-list">
          {items.map((e) => {
            const stored = storedState(e);
            return (
              <li key={e.id} class={`history-item ${stored}`}>
                <div class="meta">
                  <time dateTime={new Date(e.ts_ms).toISOString()} title={new Date(e.ts_ms).toLocaleString()}>
                    {relativeTime(e.ts_ms, now)}
                  </time>
                  {e.app && <span class="chip">{e.app}</span>}
                  {e.route && e.route !== "type" && <span class="chip">{e.route}</span>}
                  {e.word_count !== undefined && <span class="muted">{e.word_count} words</span>}
                  {e.wpm !== undefined && <span class="muted">{formatWpm(e.wpm)} wpm</span>}
                </div>
                <div class="body">
                  {stored === "stored" && <p class="text">{e.text}</p>}
                  {stored === "private" && (
                    <p class="text muted">
                      <span aria-hidden="true">🔒 </span>Not stored: recorded in privacy mode.
                    </p>
                  )}
                  {stored === "failed" && <p class="text error-text">Failed: {e.error?.message}</p>}
                </div>
                {stored === "stored" && (
                  <button type="button" class="ghost" onClick={() => void copy(e)} aria-label="Copy this dictation">
                    {copied === e.id ? "Copied" : "Copy"}
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {next !== undefined && (
        <button type="button" onClick={() => void load(next)}>
          Load more
        </button>
      )}
    </div>
  );
}
