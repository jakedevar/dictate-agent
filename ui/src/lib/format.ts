// Presentation helpers for the hub. Pure; unit-tested.

import type { HistoryAnalytics, HistoryEntry } from "./protocol";

/** `YYYY-MM-DD` of a date in UTC — the calendar the daemon's analytics use. */
export function utcDay(date: Date): string {
  return date.toISOString().slice(0, 10);
}

/** Words over the last seven UTC days, today included. */
export function wordsThisWeek(analytics: HistoryAnalytics, now: Date): number {
  const start = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() - 6));
  const from = utcDay(start);
  const to = utcDay(now);
  return analytics.words_by_day
    .filter((d) => d.day >= from && d.day <= to)
    .reduce((sum, d) => sum + d.words, 0);
}

/** The last `days` UTC days, oldest first, zero-filled. */
export function recentDays(analytics: HistoryAnalytics, now: Date, days: number): { day: string; words: number }[] {
  const byDay = new Map(analytics.words_by_day.map((d) => [d.day, d.words]));
  const out: { day: string; words: number }[] = [];
  for (let i = days - 1; i >= 0; i--) {
    const day = utcDay(new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() - i)));
    out.push({ day, words: byDay.get(day) ?? 0 });
  }
  return out;
}

export function formatCount(n: number): string {
  return new Intl.NumberFormat("en-US").format(Math.round(n));
}

export function formatWpm(wpm: number | undefined): string {
  return wpm === undefined || !Number.isFinite(wpm) ? "—" : String(Math.round(wpm));
}

/** "just now", "5 min ago", "3 h ago", "yesterday", else a date. */
export function relativeTime(tsMs: number, nowMs: number): string {
  const s = Math.max(0, Math.round((nowMs - tsMs) / 1000));
  if (s < 45) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h} h ago`;
  const d = Math.round(h / 24);
  if (d === 1) return "yesterday";
  if (d < 7) return `${d} days ago`;
  return new Date(tsMs).toISOString().slice(0, 10);
}

export type Stored = "stored" | "private" | "failed";

/**
 * Whether a history row's text exists. The daemon omits `text` both for a
 * session that failed before producing any and for one recorded under
 * privacy mode; only the error distinguishes them.
 */
export function storedState(entry: HistoryEntry): Stored {
  if (entry.text !== undefined) return "stored";
  return entry.error ? "failed" : "private";
}

export function pluralize(n: number, one: string, many = `${one}s`): string {
  return `${formatCount(n)} ${n === 1 ? one : many}`;
}

/** Split a comma-separated field into trimmed, non-empty, de-duplicated items. */
export function splitList(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of text.split(",")) {
    const item = raw.trim();
    if (item && !seen.has(item.toLowerCase())) {
      seen.add(item.toLowerCase());
      out.push(item);
    }
  }
  return out;
}
