import { useEffect, useState } from "preact/hooks";
import { asBridgeError, daemon } from "../../lib/daemon";
import { headline, mark, summarize } from "../../lib/doctor";
import type { DiagnosticsReport } from "../../lib/protocol";
import { useHub } from "../useDaemon";

/**
 * Onboarding and health in one place: every check `dictate doctor` runs, with
 * its one-line fix, problems first.
 */
export function Doctor() {
  const { connection, refresh } = useHub();
  const live = connection.status === "connected";
  const canDiagnose = live && connection.features.diagnostics !== false;
  const [report, setReport] = useState<DiagnosticsReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [running, setRunning] = useState<"quick" | "full" | null>(null);
  const [ranFull, setRanFull] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);

  const run = async (quick: boolean) => {
    setRunning(quick ? "quick" : "full");
    try {
      setReport(await daemon.diagnose(quick));
      setRanFull(!quick);
      setError(null);
    } catch (e) {
      setError(asBridgeError(e).message);
    } finally {
      setRunning(null);
    }
  };

  useEffect(() => {
    if (canDiagnose) void run(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [canDiagnose, refresh]);

  const copy = async (text: string) => {
    try {
      await daemon.copyText(text);
      setCopied(text);
      window.setTimeout(() => setCopied((c) => (c === text ? null : c)), 1500);
    } catch (e) {
      setError(asBridgeError(e).message);
    }
  };

  if (!live) {
    return (
      <div class="page">
        <header class="page-head">
          <h1>Doctor</h1>
        </header>
        <section class="card">
          <h2>Getting started</h2>
          <ol>
            <li>
              Start the daemon: <code>systemctl --user start dictated</code> (or run <code>dictated</code> in a terminal).
            </li>
            <li>
              Without it, <code>dictate doctor</code> in a terminal runs the same checks and prints a fix for each.
            </li>
            <li>This window connects on its own as soon as the daemon is up.</li>
          </ol>
        </section>
      </div>
    );
  }

  const summary = report ? summarize(report.checks) : null;
  return (
    <div class="page">
      <header class="page-head">
        <h1>Doctor</h1>
        <div class="toolbar">
          <button type="button" disabled={!canDiagnose || running !== null} onClick={() => void run(true)}>
            {running === "quick" ? "Checking…" : "Check again"}
          </button>
          <button
            type="button"
            disabled={!canDiagnose || running !== null}
            onClick={() => void run(false)}
            title="Also verifies the speech model file's checksum; takes longer"
          >
            {running === "full" ? "Running full check…" : "Full check"}
          </button>
        </div>
      </header>
      {!canDiagnose && <p class="error">This daemon does not offer diagnostics; run <code>dictate doctor</code>.</p>}
      {error && (
        <p class="error" role="alert">
          {error}
        </p>
      )}
      {summary && (
        <section class={`banner ${summary.fail > 0 ? "err" : summary.warn > 0 ? "warn" : "ok"}`} role="status">
          <div>
            <strong>{headline(summary)}.</strong>
            <div class="summary-line muted">
              <span>{summary.ok} ok</span>
              {summary.warn > 0 && <span>{summary.warn} to look at</span>}
              {summary.fail > 0 && <span>{summary.fail} failing</span>}
              {summary.skipped > 0 && <span>{summary.skipped} skipped</span>}
              <span>{ranFull ? "full check" : "quick check"}</span>
            </div>
          </div>
        </section>
      )}
      {summary && (
        <ul class="checks">
          {summary.ordered.map((c) => (
            <li key={c.id} class={`check-row ${c.status}`}>
              <span class="mark" aria-label={c.status}>
                {mark(c.status)}
              </span>
              <span class="title">{c.title}</span>
              {c.detail && <span class="detail">{c.detail}</span>}
              {c.fix && (c.status === "fail" || c.status === "warn") && (
                <span class="fix">
                  <strong>Fix:</strong> {c.fix}{" "}
                  <button type="button" class="ghost" onClick={() => void copy(c.fix ?? "")} aria-label={`Copy the fix for ${c.title}`}>
                    {copied === c.fix ? "Copied" : "Copy"}
                  </button>
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
      {!summary && running && <p class="muted">Running checks…</p>}
    </div>
  );
}
