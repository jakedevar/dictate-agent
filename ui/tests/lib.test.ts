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
  ALL_FIELDS,
  LIVE_KEYS,
  changedEntries,
  displayValue,
  getPath,
  parseInput,
  sectionsFor,
  usesLegacyGrammar,
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
    format: { enabled: true, llm: { enabled: false, models: ["example:4b", "example:1b"], min_words: 3 } },
    history: { privacy_mode: false, retention_days: null },
    whisper: { device: "cuda" },
  };
  const spec = (path: string): FieldSpec => {
    const f = ALL_FIELDS.find((x) => x.path === path);
    if (!f) throw new Error(path);
    return f;
  };
  const paths = (doc: string) => sectionsFor(values, doc).flatMap((s) => s.fields.map((f) => f.path));

  test("only keys the daemon reports are shown", () => {
    const shown = visibleSections(values).flatMap((s) => s.fields.map((f) => f.path));
    expect(shown).toContain("format.llm.models");
    expect(shown).toContain("history.retention_days"); // null is a real, unset value
    expect(shown).not.toContain("audio.capture");
  });

  test("a legacy [grammar] file is edited through [grammar], never by creating [format.llm]", () => {
    const legacy = "# mine\n[grammar]\nenabled = false\nmodel = \"example:1b\"\n";
    expect(usesLegacyGrammar(legacy)).toBe(true);
    expect(paths(legacy)).toContain("grammar.model");
    expect(paths(legacy).some((p) => p.startsWith("format.llm."))).toBe(false);
    expect(paths(legacy)).toContain("format.enabled"); // the rules are not the LLM pass
  });

  test("a file with [format.llm] (or neither section) uses the modern keys", () => {
    for (const doc of [
      "[format.llm]\nenabled = true\n",
      "[grammar]\nenabled = true\n[format.llm]\nenabled = true\n",
      "[format]\nenabled = true\nllm.enabled = true\n[grammar]\nenabled = false\n",
      "[format.llm.timeout]\nmax_ms = 900\n[grammar]\nenabled = false\n",
      "",
    ]) {
      expect(usesLegacyGrammar(doc)).toBe(false);
      expect(paths(doc)).toContain("format.llm.enabled");
      expect(paths(doc)).not.toContain("grammar.model");
    }
    // A commented-out header does not count.
    expect(usesLegacyGrammar("[grammar]\n# [format.llm]\n")).toBe(true);
  });

  test("list fields parse, display and diff as arrays", () => {
    const models = spec("format.llm.models");
    expect(parseInput(models, " a:1b, b:2b ,, a:1b")).toEqual({ ok: true, value: ["a:1b", "b:2b"] });
    expect(parseInput(models, " ").ok).toBe(false);
    expect(displayValue(models, ["a:1b", "b:2b"])).toBe("a:1b, b:2b");
    const same = new Map<string, unknown>([["format.llm.models", ["example:4b", "example:1b"]]]);
    expect(changedEntries(values, same)).toEqual([]);
    const reordered = new Map<string, unknown>([["format.llm.models", ["example:1b", "example:4b"]]]);
    expect(changedEntries(values, reordered)).toEqual([
      { path: "format.llm.models", value: ["example:1b", "example:4b"] },
    ]);
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
