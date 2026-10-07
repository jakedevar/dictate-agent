import { useEffect, useRef, useState } from "preact/hooks";
import { asBridgeError, daemon } from "../../lib/daemon";
import {
  accepted,
  describeReason,
  dismissed,
  emptyForm,
  entryFromForm,
  formFromEntry,
  sortEntries,
  type EntryForm,
} from "../../lib/dictionary";
import { formatCount } from "../../lib/format";
import type { DictionaryEntry, DictionarySuggestion } from "../../lib/protocol";
import { useHub } from "../useDaemon";

export function Dictionary() {
  const { refresh, connection } = useHub();
  const live = connection.status === "connected";
  const canWrite = live && connection.features.dictionary_write === true;
  const canRead = live && connection.features.dictionary_read !== false;

  const [entries, setEntries] = useState<DictionaryEntry[]>([]);
  const [suggestions, setSuggestions] = useState<DictionarySuggestion[]>([]);
  const [search, setSearch] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [form, setForm] = useState<EntryForm | null>(null);
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    if (!canRead) return;
    let alive = true;
    Promise.all([daemon.listDictionary(), daemon.listSuggestions().catch(() => ({ suggestions: [] }))])
      .then(([list, sugg]) => {
        if (!alive) return;
        setEntries(list.entries);
        setSuggestions(sugg.suggestions);
        setError(null);
      })
      .catch((e: unknown) => alive && setError(asBridgeError(e).message));
    return () => {
      alive = false;
    };
  }, [refresh, canRead, nonce]);

  const reload = () => setNonce((n) => n + 1);
  const flash = (text: string) => {
    setNotice(text);
    window.setTimeout(() => setNotice((n) => (n === text ? null : n)), 2500);
  };

  const save = async (entry: DictionaryEntry, message: string) => {
    try {
      await daemon.upsertEntry(entry);
      setError(null);
      flash(message);
      reload();
      return true;
    } catch (e) {
      setError(asBridgeError(e).message);
      return false;
    }
  };

  const remove = async (entry: DictionaryEntry) => {
    if (entry.id === undefined) return;
    if (!window.confirm(`Delete “${entry.phrase}” from your dictionary?`)) return;
    try {
      await daemon.deleteEntry(entry.id);
      flash(`Deleted “${entry.phrase}”.`);
      reload();
    } catch (e) {
      setError(asBridgeError(e).message);
    }
  };

  const q = search.trim().toLowerCase();
  const shown = sortEntries(entries).filter(
    (e) => !q || e.phrase.toLowerCase().includes(q) || (e.sounds_like ?? []).some((s) => s.toLowerCase().includes(q)),
  );

  return (
    <div class="page">
      <header class="page-head">
        <h1>Dictionary</h1>
        {canWrite && !form && (
          <button type="button" class="primary" onClick={() => setForm(emptyForm())}>
            Add a word
          </button>
        )}
      </header>
      <p class="muted">
        Words and names dictation should always get right. Each one also nudges recognition toward it, and “sounds like”
        lists the ways it tends to be misheard.
      </p>
      {!canRead && live && <p class="error">This daemon does not offer the dictionary.</p>}
      {error && (
        <p class="error" role="alert">
          {error}
        </p>
      )}
      {notice && (
        <p class="notice" role="status">
          {notice}
        </p>
      )}

      {form && (
        <EntryEditor
          form={form}
          existing={entries.find((e) => e.id === form.id)}
          onCancel={() => setForm(null)}
          onSave={async (entry) => {
            if (await save(entry, form.id === undefined ? `Added “${entry.phrase}”.` : `Saved “${entry.phrase}”.`)) {
              setForm(null);
            }
          }}
        />
      )}

      {suggestions.length > 0 && (
        <section class="card" aria-labelledby="suggestions-title">
          <div class="card-head">
            <h2 id="suggestions-title">Suggestions</h2>
            <span class="muted small">Dismissed suggestions are kept as disabled entries, so they are not suggested again.</span>
          </div>
          <ul class="suggestions">
            {suggestions.map((s) => (
              <li key={`${s.entry.phrase}-${s.reason}`}>
                <div class="what">
                  <strong>{s.entry.phrase}</strong>
                  {(s.entry.sounds_like ?? []).length > 0 && (
                    <span class="muted"> — heard as {(s.entry.sounds_like ?? []).join(", ")}</span>
                  )}
                  <div class="muted small">
                    {describeReason(s.reason)} · {formatCount(s.count)} times over {s.days} {s.days === 1 ? "day" : "days"}
                  </div>
                </div>
                <button
                  type="button"
                  class="primary"
                  disabled={!canWrite}
                  onClick={() => void save(accepted(s), `Added “${s.entry.phrase}”.`)}
                >
                  Accept
                </button>
                <button
                  type="button"
                  class="ghost"
                  disabled={!canWrite}
                  onClick={() => void save(dismissed(s), `Dismissed “${s.entry.phrase}”.`)}
                >
                  Dismiss
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}

      <div class="toolbar">
        <label class="search">
          <span class="visually-hidden">Filter the dictionary</span>
          <input
            type="search"
            placeholder="Filter…"
            value={search}
            onInput={(e) => setSearch((e.target as HTMLInputElement).value)}
          />
        </label>
        <span class="muted small">
          {formatCount(entries.length)} {entries.length === 1 ? "entry" : "entries"}
        </span>
      </div>

      {shown.length === 0 ? (
        <p class="muted">{q ? `Nothing matches “${search.trim()}”.` : "Your dictionary is empty."}</p>
      ) : (
        <table class="dict-table">
          <thead>
            <tr>
              <th scope="col">Phrase</th>
              <th scope="col">Sounds like</th>
              <th scope="col">Apps</th>
              <th scope="col" class="num">
                Used
              </th>
              <th scope="col">On</th>
              <th scope="col">
                <span class="visually-hidden">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {shown.map((e) => (
              <tr key={e.id ?? e.phrase} class={e.enabled === false ? "disabled" : ""}>
                <td class="phrase">
                  {e.phrase}
                  {e.case_sensitive && <span class="chip" title="Case-sensitive">Aa</span>}
                  {e.source && e.source !== "manual" && <span class="chip">{e.source.replace(/_/g, " ")}</span>}
                </td>
                <td>{(e.sounds_like ?? []).join(", ") || <span class="muted">—</span>}</td>
                <td>{(e.apps ?? []).join(", ") || <span class="muted">all</span>}</td>
                <td class="num">{e.hit_count !== undefined ? formatCount(e.hit_count) : "—"}</td>
                <td>
                  <input
                    type="checkbox"
                    checked={e.enabled !== false}
                    disabled={!canWrite}
                    aria-label={`${e.enabled !== false ? "Disable" : "Enable"} “${e.phrase}”`}
                    onChange={() =>
                      void save(
                        { ...e, enabled: e.enabled === false },
                        `${e.enabled === false ? "Enabled" : "Disabled"} “${e.phrase}”.`,
                      )
                    }
                  />
                </td>
                <td class="actions">
                  <button type="button" class="ghost" disabled={!canWrite} onClick={() => setForm(formFromEntry(e))}>
                    Edit
                  </button>
                  <button type="button" class="ghost danger" disabled={!canWrite} onClick={() => void remove(e)}>
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

function EntryEditor(props: {
  form: EntryForm;
  existing: DictionaryEntry | undefined;
  onSave: (entry: DictionaryEntry) => Promise<void>;
  onCancel: () => void;
}) {
  const [form, setForm] = useState<EntryForm>(props.form);
  const [problem, setProblem] = useState<{ field: string; error: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const first = useRef<HTMLInputElement>(null);
  useEffect(() => first.current?.focus(), []);

  const set = <K extends keyof EntryForm>(key: K, value: EntryForm[K]) => setForm((f) => ({ ...f, [key]: value }));

  const submit = async (e: Event) => {
    e.preventDefault();
    const result = entryFromForm(form, props.existing);
    if (!result.ok) {
      setProblem({ field: result.field, error: result.error });
      return;
    }
    setProblem(null);
    setBusy(true);
    await props.onSave(result.entry);
    setBusy(false);
  };

  return (
    <form
      class="card entry-form"
      onSubmit={(e) => void submit(e)}
      onKeyDown={(e) => e.key === "Escape" && props.onCancel()}
      aria-label={form.id === undefined ? "Add a dictionary entry" : "Edit a dictionary entry"}
    >
      <label class="field">
        <span>Phrase</span>
        <input
          ref={first}
          type="text"
          value={form.phrase}
          aria-invalid={problem?.field === "phrase"}
          onInput={(e) => set("phrase", (e.target as HTMLInputElement).value)}
          placeholder="e.g. Kubernetes"
        />
        {problem?.field === "phrase" && <span class="error">{problem.error}</span>}
      </label>
      <label class="field">
        <span>Sounds like</span>
        <input
          type="text"
          value={form.soundsLike}
          aria-invalid={problem?.field === "soundsLike"}
          onInput={(e) => set("soundsLike", (e.target as HTMLInputElement).value)}
          placeholder="e.g. cuban eighties, kubernetties"
        />
        <span class="help">Comma-separated ways it gets misheard.</span>
        {problem?.field === "soundsLike" && <span class="error">{problem.error}</span>}
      </label>
      <label class="field wide">
        <span>Only in these apps</span>
        <input
          type="text"
          value={form.apps}
          onInput={(e) => set("apps", (e.target as HTMLInputElement).value)}
          placeholder="Leave empty for every app"
        />
        <span class="help">Comma-separated window classes, e.g. ghostty, google-chrome.</span>
      </label>
      <label class="check">
        <input
          type="checkbox"
          checked={form.caseSensitive}
          onChange={(e) => set("caseSensitive", (e.target as HTMLInputElement).checked)}
        />
        Case-sensitive
      </label>
      <label class="check">
        <input
          type="checkbox"
          checked={form.enabled}
          onChange={(e) => set("enabled", (e.target as HTMLInputElement).checked)}
        />
        Enabled
      </label>
      <div class="form-actions wide">
        <button type="button" onClick={props.onCancel}>
          Cancel
        </button>
        <button type="submit" class="primary" disabled={busy}>
          {form.id === undefined ? "Add" : "Save"}
        </button>
      </div>
    </form>
  );
}
