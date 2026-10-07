import { describe, expect, test } from "bun:test";
import {
  accepted,
  describeReason,
  dismissed,
  emptyForm,
  entryFromForm,
  formFromEntry,
  sortEntries,
} from "../src/lib/dictionary";
import { headline, mark, summarize } from "../src/lib/doctor";
import type { DiagnosticCheck, DictionarySuggestion } from "../src/lib/protocol";

const suggestion: DictionarySuggestion = {
  entry: { phrase: "Kubernetes", sounds_like: ["cube ernetties"], enabled: true, hit_count: 4, id: 99 },
  reason: "consistent_rewrite",
  count: 5,
  days: 3,
  first_seen: "2026-09-01T00:00:00Z",
  last_seen: "2026-09-03T00:00:00Z",
};

describe("dictionary model", () => {
  test("a form round-trips an entry", () => {
    const entry = { id: 7, phrase: "Synthetic", sounds_like: ["sin thetic"], apps: ["ghostty"], case_sensitive: true, enabled: false, source: "auto_learned" };
    const result = entryFromForm(formFromEntry(entry), entry);
    expect(result).toEqual({ ok: true, entry });
  });

  test("an empty phrase and a no-op spoken form are refused before sending", () => {
    expect(entryFromForm({ ...emptyForm(), phrase: "  " })).toMatchObject({ ok: false, field: "phrase" });
    expect(entryFromForm({ ...emptyForm(), phrase: "Rust", soundsLike: "rust, rusty" })).toMatchObject({
      ok: false,
      field: "soundsLike",
    });
  });

  test("a new entry carries no id and lists are split", () => {
    const r = entryFromForm({ ...emptyForm(), phrase: " Tauri ", soundsLike: "tory, towery", apps: "" });
    expect(r).toEqual({
      ok: true,
      entry: { phrase: "Tauri", sounds_like: ["tory", "towery"], apps: [], case_sensitive: false, enabled: true },
    });
  });

  test("accept adds an enabled entry; dismiss stores a disabled one so it is not suggested again", () => {
    expect(accepted(suggestion)).toEqual({ phrase: "Kubernetes", sounds_like: ["cube ernetties"], enabled: true });
    expect(dismissed(suggestion)).toEqual({ phrase: "Kubernetes", sounds_like: ["cube ernetties"], enabled: false });
  });

  test("entries sort enabled first, then by phrase", () => {
    const sorted = sortEntries([
      { phrase: "zed", enabled: true },
      { phrase: "Alpha", enabled: false },
      { phrase: "beta" },
    ]).map((e) => e.phrase);
    expect(sorted).toEqual(["beta", "zed", "Alpha"]);
  });

  test("reasons read as sentences", () => {
    expect(describeReason("recurring_term")).toBe("A term you use often");
    expect(describeReason("some_new_reason")).toBe("some new reason");
  });
});

describe("doctor model", () => {
  const check = (id: string, status: string): DiagnosticCheck => ({ id, title: id, status, detail: "" });

  test("failures come first, then warnings, keeping the daemon's order within each", () => {
    const s = summarize([check("a", "ok"), check("b", "warn"), check("c", "fail"), check("d", "ok"), check("e", "fail")]);
    expect(s.ordered.map((c) => c.id)).toEqual(["c", "e", "b", "a", "d"]);
    expect([s.fail, s.warn, s.ok, s.skipped]).toEqual([2, 1, 2, 0]);
  });

  test("an unknown status is treated as something to look at, never as ok", () => {
    const s = summarize([check("x", "degraded")]);
    expect(s.warn).toBe(1);
    expect(mark("degraded")).toBe("!");
  });

  test("headline", () => {
    expect(headline(summarize([check("a", "fail")]))).toContain("1 problem");
    expect(headline(summarize([check("a", "warn"), check("b", "warn")]))).toContain("2 things");
    expect(headline(summarize([check("a", "ok")]))).toBe("Everything checks out");
  });
});
