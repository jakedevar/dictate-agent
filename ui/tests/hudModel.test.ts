import { describe, expect, test } from "bun:test";
import {
  METER_BARS,
  formatElapsed,
  initialView,
  label,
  normalizeLevel,
  onConnection,
  reduce,
  shorten,
  type HudView,
} from "../src/lib/hudModel";
import type { DaemonEvent } from "../src/lib/protocol";

const state = (to: string, session = "s1"): DaemonEvent => ({
  type: "state_changed",
  session_id: session,
  from: "idle",
  to,
});
const level = (rms: number): DaemonEvent => ({ type: "audio_level", session_id: "s1", rms });

function run(events: DaemonEvent[], start: HudView = initialView()): HudView {
  return events.reduce((v, e, i) => reduce(v, e, 1000 + i), start);
}

describe("the Flow bar model", () => {
  test("recording starts a fresh session with an empty meter and a clock", () => {
    const v = run([state("recording")]);
    expect(v.phase).toBe("recording");
    expect(v.startedAt).toBe(1000);
    expect(v.levels).toHaveLength(METER_BARS);
    expect(v.levels.every((l) => l === 0)).toBe(true);
  });

  test("levels scroll in from the right while recording", () => {
    const v = run([state("recording"), level(0.25), level(0.0625)]);
    expect(v.levels).toHaveLength(METER_BARS);
    expect(v.levels.at(-1)).toBeCloseTo(0.5);
    expect(v.levels.at(-2)).toBeCloseTo(1);
  });

  test("levels outside recording are ignored", () => {
    const v = run([state("recording"), state("transcribing"), level(0.25)]);
    expect(v.levels.every((l) => l === 0)).toBe(true);
  });

  test("the walk through processing to done carries the word count", () => {
    const v = run([
      state("recording"),
      state("transcribing"),
      state("formatting"),
      { type: "final", session_id: "s1", text: "synthetic words here", word_count: 3 },
      state("done"),
    ]);
    expect(v.phase).toBe("done");
    expect(label(v)).toBe("3 words");
  });

  test("an error shows a short readable message", () => {
    const v = run([
      state("recording"),
      state("transcribing"),
      {
        type: "error",
        session_id: "s1",
        error: { code: "stt_failed", message: "the speech model could not be loaded from the configured path" },
      },
      state("error"),
    ]);
    expect(v.phase).toBe("error");
    expect(label(v).length).toBeLessThanOrEqual(38);
    expect(label(v).endsWith("…")).toBe(true);
  });

  test("a terminal state with nothing on screen stays hidden", () => {
    expect(run([state("done")]).phase).toBe("hidden");
    expect(run([state("cancelled")]).phase).toBe("hidden");
  });

  test("idle after a result keeps the result showing for its linger", () => {
    const v = run([state("recording"), state("cancelled"), state("idle")]);
    expect(v.phase).toBe("cancelled");
  });

  test("an upload that starts at transcribing shows progress without a meter", () => {
    const v = run([state("transcribing", "upload-1")]);
    expect(v.phase).toBe("transcribing");
    expect(v.startedAt).toBeNull();
  });

  test("a new session replaces a lingering result", () => {
    const v = run([state("recording", "a"), state("done", "a"), state("recording", "b"), level(0.25)]);
    expect(v.sessionId).toBe("b");
    expect(v.phase).toBe("recording");
    expect(v.words).toBeNull();
  });

  test("events of unknown types change nothing", () => {
    const before = run([state("recording")]);
    expect(reduce(before, { type: "wake_word_heard", keyword: "x" }, 5000)).toBe(before);
  });

  test("losing the daemon clears the bar", () => {
    const v = run([state("recording")]);
    expect(onConnection(v, { status: "disconnected", reason: "x", retry_in_ms: 250, attempt: 1 }).phase).toBe(
      "hidden",
    );
  });
});

describe("helpers", () => {
  test("level normalization is monotone, bounded and robust", () => {
    expect(normalizeLevel(0)).toBe(0);
    expect(normalizeLevel(-1)).toBe(0);
    expect(normalizeLevel(Number.NaN)).toBe(0);
    expect(normalizeLevel(0.25)).toBe(1);
    expect(normalizeLevel(5)).toBe(1);
    expect(normalizeLevel(0.02)).toBeGreaterThan(0.25); // quiet speech is visible
    expect(normalizeLevel(0.05)).toBeGreaterThan(normalizeLevel(0.02));
  });

  test("elapsed time reads m:ss", () => {
    expect(formatElapsed(0)).toBe("0:00");
    expect(formatElapsed(7_900)).toBe("0:07");
    expect(formatElapsed(65_000)).toBe("1:05");
    expect(formatElapsed(-5)).toBe("0:00");
  });

  test("shorten collapses whitespace and ellipsizes", () => {
    expect(shorten("  a \n b  ", 10)).toBe("a b");
    expect(shorten("abcdefghij", 5)).toBe("abcd…");
  });

  test("labels", () => {
    expect(label({ ...initialView(), phase: "formatting" })).toBe("Polishing");
    expect(label({ ...initialView(), phase: "done", words: 1 })).toBe("1 word");
    expect(label({ ...initialView(), phase: "done" })).toBe("Done");
  });
});
