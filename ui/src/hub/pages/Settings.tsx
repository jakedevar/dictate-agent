import { useEffect, useState } from "preact/hooks";
import { asBridgeError, daemon } from "../../lib/daemon";
import type { BridgeError, ConfigSnapshot } from "../../lib/protocol";
import {
  LIVE_KEYS,
  changedEntries,
  displayValue,
  getPath,
  parseInput,
  sectionsFor,
  type FieldSpec,
} from "../../lib/settings";
import { useHub } from "../useDaemon";

type Tab = "common" | "toml";

/** What the last save or validation said. */
type Outcome =
  | { kind: "saved"; applied: string[]; restart: string[]; live: string[] }
  | { kind: "unchanged" }
  | { kind: "error"; error: BridgeError }
  | { kind: "conflict" };

export function Settings() {
  const { connection } = useHub();
  const live = connection.status === "connected";
  const canRead = live && connection.features.config_read === true;
  const canWrite = live && connection.features.config_write === true;

  const [snapshot, setSnapshot] = useState<ConfigSnapshot | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("common");
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    if (!canRead) return;
    let alive = true;
    daemon
      .getConfig()
      .then((s) => {
        if (!alive) return;
        setSnapshot(s);
        setLoadError(null);
      })
      .catch((e: unknown) => alive && setLoadError(asBridgeError(e).message));
    return () => {
      alive = false;
    };
  }, [canRead, nonce]);

  const reload = () => setNonce((n) => n + 1);

  return (
    <div class="page">
      <header class="page-head">
        <h1>Settings</h1>
        {snapshot?.file && (
          <span class="muted small" title="The daemon's configuration file">
            <code>{snapshot.file.path}</code>
          </span>
        )}
      </header>
      {live && !canRead && <p class="error">This daemon does not offer its configuration over this connection.</p>}
      {loadError && (
        <p class="error" role="alert">
          {loadError}
        </p>
      )}
      {snapshot && snapshot.errors && snapshot.errors.length > 0 && (
        <section class="banner err" role="alert">
          <div>
            <strong>The file on disk would not load.</strong> The daemon would refuse to start with it; fix it in the
            TOML tab.
            <ul class="messages">
              {snapshot.errors.map((e) => (
                <li key={e}>{e}</li>
              ))}
            </ul>
          </div>
        </section>
      )}
      {snapshot && (snapshot.restart_required ?? []).length > 0 && (
        <section class="banner warn" role="status">
          <div>
            <strong>Restart dictated to apply saved changes:</strong>{" "}
            {(snapshot.restart_required ?? []).map((k) => (
              <code key={k}>{k}</code>
            ))}
            <div class="muted small">
              <code>systemctl --user restart dictated</code>
            </div>
          </div>
        </section>
      )}
      <div class="tabs" role="tablist" aria-label="Editor">
        <button type="button" role="tab" aria-selected={tab === "common"} onClick={() => setTab("common")}>
          Common settings
        </button>
        <button type="button" role="tab" aria-selected={tab === "toml"} onClick={() => setTab("toml")}>
          TOML
        </button>
      </div>
      {snapshot &&
        (tab === "common" ? (
          <StructuredEditor snapshot={snapshot} canWrite={canWrite} onSaved={setSnapshot} onConflict={reload} />
        ) : (
          <RawEditor snapshot={snapshot} canWrite={canWrite} onSaved={setSnapshot} onConflict={reload} />
        ))}
      {snapshot && (snapshot.warnings ?? []).length > 0 && (
        <details class="card">
          <summary>
            {snapshot.warnings?.length} loader {snapshot.warnings?.length === 1 ? "warning" : "warnings"}
          </summary>
          <ul class="messages">
            {(snapshot.warnings ?? []).map((w) => (
              <li key={w}>{w}</li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}

interface EditorProps {
  snapshot: ConfigSnapshot;
  canWrite: boolean;
  onSaved: (s: ConfigSnapshot) => void;
  onConflict: () => void;
}

function StructuredEditor({ snapshot, canWrite, onSaved, onConflict }: EditorProps) {
  const [inputs, setInputs] = useState<Map<string, string | boolean>>(new Map());
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const sections = sectionsFor(snapshot.values, snapshot.file?.document ?? "");

  // A fresh snapshot (after a save or a reload) clears the edits.
  useEffect(() => setInputs(new Map()), [snapshot]);

  const fieldError = (spec: FieldSpec): string | null => {
    const raw = inputs.get(spec.path);
    if (raw === undefined) return null;
    const parsed = parseInput(spec, raw);
    return parsed.ok ? null : parsed.error;
  };
  const allSpecs = sections.flatMap((s) => s.fields);
  const invalid = allSpecs.some((s) => fieldError(s) !== null);
  const edits = new Map<string, unknown>();
  for (const spec of allSpecs) {
    const raw = inputs.get(spec.path);
    if (raw === undefined) continue;
    const parsed = parseInput(spec, raw);
    if (parsed.ok) edits.set(spec.path, parsed.value);
  }
  const entries = changedEntries(snapshot.values, edits);
  const serverPath = outcome?.kind === "error" ? (outcome.error.detail?.path ?? null) : null;

  const save = async () => {
    if (entries.length === 0) {
      setOutcome({ kind: "unchanged" });
      return;
    }
    setBusy(true);
    try {
      const result = await daemon.setConfig({
        entries,
        ...(snapshot.file?.revision ? { base_revision: snapshot.file.revision } : {}),
      });
      const applied = result.applied ?? [];
      const restart = result.restart_required ?? [];
      setOutcome({ kind: "saved", applied, restart, live: applied.filter((k) => LIVE_KEYS.has(k)) });
      onSaved(result);
    } catch (e) {
      const error = asBridgeError(e);
      if (error.code === "conflict") {
        setOutcome({ kind: "conflict" });
        onConflict();
      } else {
        setOutcome({ kind: "error", error });
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      class="page"
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      {sections.map((section) => (
        <section class="card" key={section.title} aria-labelledby={`sec-${section.title}`}>
          <div class="card-head">
            <h2 id={`sec-${section.title}`}>{section.title}</h2>
          </div>
          <div class="settings-grid">
            {section.fields.map((spec) => {
              const current = inputs.get(spec.path) ?? displayValue(spec, getPath(snapshot.values, spec.path));
              const error = fieldError(spec) ?? (serverPath === spec.path ? (outcome as { error: BridgeError }).error.message : null);
              const id = `f-${spec.path}`;
              return [
                <label for={id} key={`${spec.path}-l`}>
                  {spec.label}
                  {LIVE_KEYS.has(spec.path) ? (
                    <span class="tag-live" title="Applies immediately">
                      live
                    </span>
                  ) : null}
                </label>,
                <div class="control" key={`${spec.path}-c`}>
                  <Input
                    id={id}
                    spec={spec}
                    value={current}
                    disabled={!canWrite}
                    invalid={error !== null}
                    onChange={(v) => setInputs((m) => new Map(m).set(spec.path, v))}
                  />
                  {error ? <span class="error small">{error}</span> : spec.help ? <span class="muted small">{spec.help}</span> : null}
                </div>,
              ];
            })}
          </div>
        </section>
      ))}
      <div class="form-actions">
        <span class="status-text" role="status">
          <OutcomeText outcome={outcome} />
        </span>
        <button type="button" disabled={inputs.size === 0 || busy} onClick={() => setInputs(new Map())}>
          Revert
        </button>
        <button type="submit" class="primary" disabled={!canWrite || busy || invalid || entries.length === 0}>
          {busy ? "Saving…" : entries.length > 0 ? `Save ${entries.length} ${entries.length === 1 ? "change" : "changes"}` : "Save"}
        </button>
      </div>
    </form>
  );
}

function Input(props: {
  id: string;
  spec: FieldSpec;
  value: string | boolean;
  disabled: boolean;
  invalid: boolean;
  onChange: (v: string | boolean) => void;
}) {
  const { id, spec, value, disabled, invalid, onChange } = props;
  if (spec.kind === "bool") {
    return (
      <input
        id={id}
        type="checkbox"
        checked={value === true}
        disabled={disabled}
        onChange={(e) => onChange((e.target as HTMLInputElement).checked)}
      />
    );
  }
  if (spec.kind === "select") {
    const options = spec.options ?? [];
    const v = String(value);
    return (
      <select id={id} value={v} disabled={disabled} onChange={(e) => onChange((e.target as HTMLSelectElement).value)}>
        {!options.includes(v) && <option value={v}>{v}</option>}
        {options.map((o) => (
          <option key={o} value={o}>
            {o}
          </option>
        ))}
      </select>
    );
  }
  const onInput = (e: Event) => onChange((e.target as HTMLInputElement).value);
  if (spec.kind === "number" || spec.kind === "optional-number") {
    return (
      <input
        id={id}
        type="number"
        {...(spec.min !== undefined ? { min: spec.min } : {})}
        {...(spec.step !== undefined ? { step: spec.step } : {})}
        value={String(value)}
        disabled={disabled}
        aria-invalid={invalid}
        onInput={onInput}
      />
    );
  }
  return (
    <input id={id} type="text" value={String(value)} disabled={disabled} aria-invalid={invalid} onInput={onInput} />
  );
}

function OutcomeText({ outcome }: { outcome: Outcome | null }) {
  if (!outcome) return null;
  switch (outcome.kind) {
    case "unchanged":
      return <span class="muted">Nothing to save.</span>;
    case "conflict":
      return (
        <span class="error">
          The file changed on disk since it was loaded, so nothing was written. It has been reloaded; apply your change
          again.
        </span>
      );
    case "error":
      return <span class="error">{outcome.error.message}</span>;
    case "saved": {
      if (outcome.applied.length === 0) return <span class="muted">Saved; no setting changed value.</span>;
      const needsRestart = outcome.restart.length > 0;
      return (
        <span class="notice">
          Saved.{" "}
          {outcome.live.length > 0 && `${outcome.live.join(", ")} applied now. `}
          {needsRestart && <span class="tag-restart">Restart dictated to apply the rest.</span>}
        </span>
      );
    }
  }
}

function RawEditor({ snapshot, canWrite, onSaved, onConflict }: EditorProps) {
  const original = snapshot.file?.document ?? "";
  const [text, setText] = useState(original);
  const [check, setCheck] = useState<{ ok: true; result: ConfigSnapshot } | { ok: false; error: BridgeError } | null>(null);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);

  useEffect(() => {
    setText(snapshot.file?.document ?? "");
    setCheck(null);
  }, [snapshot]);

  // Validate as you type: a dry run through the daemon's own loader.
  useEffect(() => {
    if (text === original) {
      setCheck(null);
      return;
    }
    let alive = true;
    const t = window.setTimeout(() => {
      daemon
        .setConfig({ document: text, dry_run: true })
        .then((result) => alive && setCheck({ ok: true, result }))
        .catch((e: unknown) => alive && setCheck({ ok: false, error: asBridgeError(e) }));
    }, 400);
    return () => {
      alive = false;
      window.clearTimeout(t);
    };
  }, [text, original]);

  const save = async () => {
    setBusy(true);
    try {
      const result = await daemon.setConfig({
        document: text,
        ...(snapshot.file?.revision ? { base_revision: snapshot.file.revision } : {}),
      });
      const applied = result.applied ?? [];
      setOutcome({
        kind: "saved",
        applied,
        restart: result.restart_required ?? [],
        live: applied.filter((k) => LIVE_KEYS.has(k)),
      });
      onSaved(result);
    } catch (e) {
      const error = asBridgeError(e);
      if (error.code === "conflict") {
        setOutcome({ kind: "conflict" });
        onConflict();
      } else {
        setOutcome({ kind: "error", error });
      }
    } finally {
      setBusy(false);
    }
  };

  const invalid = check !== null && !check.ok;
  return (
    <section class="card">
      <label class="field">
        <span>config.toml</span>
        <textarea
          rows={22}
          spellcheck={false}
          value={text}
          disabled={!canWrite}
          aria-invalid={invalid}
          aria-describedby="toml-check"
          onInput={(e) => setText((e.target as HTMLTextAreaElement).value)}
        />
      </label>
      <div id="toml-check" class="small" role="status">
        {check === null ? (
          <span class="muted">Comments and unknown keys are kept exactly as written. Changes are checked as you type.</span>
        ) : check.ok ? (
          <span class="notice">
            Valid.{" "}
            {(check.result.applied ?? []).length > 0
              ? `Changes: ${(check.result.applied ?? []).join(", ")}.`
              : "No setting changes value."}
            {(check.result.warnings ?? []).length > 0 && (
              <ul class="messages muted">
                {(check.result.warnings ?? []).map((w) => (
                  <li key={w}>{w}</li>
                ))}
              </ul>
            )}
          </span>
        ) : (
          <span class="error">
            {check.error.message}
            {(check.error.detail?.errors ?? []).length > 1 && (
              <ul class="messages">
                {(check.error.detail?.errors ?? []).slice(1).map((e) => (
                  <li key={e}>{e}</li>
                ))}
              </ul>
            )}
          </span>
        )}
      </div>
      <div class="form-actions">
        <span class="status-text" role="status">
          <OutcomeText outcome={outcome} />
        </span>
        <button type="button" disabled={text === original || busy} onClick={() => setText(original)}>
          Revert
        </button>
        <button
          type="button"
          class="primary"
          disabled={!canWrite || busy || text === original || invalid}
          onClick={() => void save()}
        >
          {busy ? "Saving…" : "Save file"}
        </button>
      </div>
    </section>
  );
}
