import { describe, expect, test } from "bun:test";
import {
  formatWpm,
  recentDays,
  relativeTime,
  splitList,
  storedState,
  utcDay,
  wordsThisWeek,
} from "../src/lib/format";
import type { HistoryAnalytics } from "../src/lib/protocol";
import {
  LIVE_KEYS,
  SECTIONS,
  changedEntries,
  displayValue,
  getPath,
  parseInput,
  visibleSections,
  type FieldSpec,
} from "../src/lib/settings";

const analytics: HistoryAnalytics = {
  type: "history_analytics",
  overall_wpm: 142.4,
  words_today: 120,
  words_by_day: [
    { day: "2026-09-20", words: 999 }, // outside the week
    { day: "2026-09-24", words: 10 },
    { day: "2026-09-29", words: 30 },
    { day: "2026-09-30", words: 120 },
  ],
  current_streak_days: 2,
  longest_streak_days: 9,
};
const NOW = new Date("2026-09-30T15:00:00Z");

describe("analytics", () => {
  test("words this week covers seven UTC days including today", () => {
    expect(wordsThisWeek(analytics, NOW)).toBe(160);
  });

  test("recent days are zero-filled and ordered", () => {
    const days = recentDays(analytics, NOW, 3);
    expect(days).toEqual([
      { day: "2026-09-28", words: 0 },
      { day: "2026-09-29", words: 30 },
      { day: "2026-09-30", words: 120 },
    ]);
  });

  test("utc day ignores the local timezone", () => {
    expect(utcDay(new Date("2026-09-30T23:59:59Z"))).toBe("2026-09-30");
  });

  test("wpm formatting", () => {
    expect(formatWpm(142.4)).toBe("142");
    expect(formatWpm(undefined)).toBe("—");
  });

  test("relative time", () => {
    const now = Date.parse("2026-09-30T15:00:00Z");
    expect(relativeTime(now - 10_000, now)).toBe("just now");
    expect(relativeTime(now - 5 * 60_000, now)).toBe("5 min ago");
    expect(relativeTime(now - 3 * 3_600_000, now)).toBe("3 h ago");
    expect(relativeTime(now - 26 * 3_600_000, now)).toBe("yesterday");
    expect(relativeTime(now - 30 * 86_400_000, now)).toBe("2026-08-31");
  });
});

describe("history privacy", () => {
  const base = { id: 1, session_id: "s", ts_ms: 0 };
  test("absent text is private unless the session failed", () => {
    expect(storedState({ ...base, text: "synthetic" })).toBe("stored");
    expect(storedState({ ...base, text: "" })).toBe("stored");
    expect(storedState(base)).toBe("private");
    expect(storedState({ ...base, error: { code: "stt_failed", message: "x" } })).toBe("failed");
  });
});

describe("list fields", () => {
  test("split trims, drops empties and case-insensitive duplicates", () => {
    expect(splitList(" kubernetties, cube ernetties,, Kubernetties ")).toEqual(["kubernetties", "cube ernetties"]);
    expect(splitList("")).toEqual([]);
  });
});

describe("settings model", () => {
  const values = {
    grammar: { enabled: false, model: "example:1b", min_words: 3, timeout_s: 10.0 },
    history: { privacy_mode: false, retention_days: null },
    whisper: { device: "cuda" },
  };
  const spec = (path: string): FieldSpec => {
    const f = SECTIONS.flatMap((s) => s.fields).find((x) => x.path === path);
    if (!f) throw new Error(path);
    return f;
  };

  test("only keys the daemon reports are shown", () => {
    const shown = visibleSections(values).flatMap((s) => s.fields.map((f) => f.path));
    expect(shown).toContain("grammar.model");
    expect(shown).toContain("history.retention_days"); // null is a real, unset value
    expect(shown).not.toContain("audio.capture");
  });

  test("an untouched form sends nothing, and only real changes are sent", () => {
    const edits = new Map<string, unknown>([
      ["grammar.enabled", false],
      ["grammar.model", "example:4b"],
      ["history.retention_days", null],
    ]);
    expect(changedEntries(values, edits)).toEqual([{ path: "grammar.model", value: "example:4b" }]);
    expect(changedEntries(values, new Map())).toEqual([]);
  });

  test("inputs parse to the JSON the daemon expects", () => {
    expect(parseInput(spec("grammar.enabled"), true)).toEqual({ ok: true, value: true });
    expect(parseInput(spec("grammar.min_words"), "5")).toEqual({ ok: true, value: 5 });
    expect(parseInput(spec("grammar.min_words"), "2.5")).toEqual({ ok: false, error: "a whole number" });
    expect(parseInput(spec("grammar.min_words"), "")).toEqual({ ok: false, error: "required" });
    expect(parseInput(spec("grammar.timeout_s"), "2.5")).toEqual({ ok: true, value: 2.5 });
    expect(parseInput(spec("grammar.timeout_s"), "0")).toEqual({ ok: false, error: "at least 0.1" });
    expect(parseInput(spec("history.retention_days"), " ")).toEqual({ ok: true, value: null });
    expect(parseInput(spec("whisper.device"), "tpu").ok).toBe(false);
  });

  test("display values", () => {
    expect(displayValue(spec("history.retention_days"), null)).toBe("");
    expect(displayValue(spec("grammar.enabled"), true)).toBe(true);
    expect(getPath(values, "grammar.model")).toBe("example:1b");
    expect(getPath(values, "grammar.nope")).toBeUndefined();
  });

  test("the live-key list matches the daemon's", () => {
    expect([...LIVE_KEYS]).toEqual(["history.privacy_mode"]);
  });
});
