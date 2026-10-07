// The Doctor page's model. Problems first, so a missing formatter model or an
// unreachable Ollama is the first thing on screen, not the eighth.

import type { DiagnosticCheck } from "./protocol";

const RANK: Record<string, number> = { fail: 0, warn: 1, ok: 2, skipped: 3 };

export interface Summary {
  fail: number;
  warn: number;
  ok: number;
  skipped: number;
  /** Checks ordered fail, warn, ok, skipped; the daemon's order within each. */
  ordered: DiagnosticCheck[];
}

export function summarize(checks: readonly DiagnosticCheck[]): Summary {
  const counts = { fail: 0, warn: 0, ok: 0, skipped: 0 };
  for (const c of checks) {
    const key = c.status in counts ? (c.status as keyof typeof counts) : "warn";
    counts[key] += 1;
  }
  const ordered = checks
    .map((c, i) => ({ c, i }))
    .sort((a, b) => (RANK[a.c.status] ?? 1) - (RANK[b.c.status] ?? 1) || a.i - b.i)
    .map(({ c }) => c);
  return { ...counts, ordered };
}

/** A one-line verdict for the page header. */
export function headline(s: Summary): string {
  if (s.fail > 0) return `${s.fail} ${s.fail === 1 ? "problem stops" : "problems stop"} dictation from working fully`;
  if (s.warn > 0) return `Working, with ${s.warn} ${s.warn === 1 ? "thing" : "things"} to look at`;
  return "Everything checks out";
}

export function mark(status: string): string {
  switch (status) {
    case "ok":
      return "✓";
    case "fail":
      return "✗";
    case "skipped":
      return "–";
    default:
      return "!";
  }
}
