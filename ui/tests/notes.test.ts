import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { emptyMessage, newestFirst, nextPhase, without } from "../src/lib/notes";
import type { DaemonEvent, Note } from "../src/lib/protocol";

const note = (id: number, ts_ms: number, text = `note ${id}`): Note => ({ id, ts_ms, text, word_count: 2 });
const state = (to: string): DaemonEvent => ({ type: "state_changed", session_id: "s", from: "idle", to });

describe("notes model", () => {
  test("newest first, ties broken by the higher id, without mutating the input", () => {
    const input = [note(1, 100), note(3, 300), note(2, 300)];
    expect(newestFirst(input).map((n) => n.id)).toEqual([3, 2, 1]);
    expect(input.map((n) => n.id)).toEqual([1, 3, 2]);
  });

  test("deleting removes exactly one note", () => {
    const list = [note(1, 1), note(2, 2)];
    expect(without(list, 1).map((n) => n.id)).toEqual([2]);
    expect(without(list, 99)).toEqual(list);
  });

  test("the empty state teaches the triggers, and a search says what it missed", () => {
    expect(emptyMessage("")).toContain("note to self");
    expect(emptyMessage("milk")).toBe("No notes match “milk”.");
  });
});

describe("dictate-a-note button phase", () => {
  test("follows recording, then the pipeline, then a terminal state", () => {
    let phase = nextPhase("idle", state("recording"));
    expect(phase).toBe("recording");
    for (const stage of ["transcribing", "formatting", "injecting"]) {
      phase = nextPhase(phase, state(stage));
      expect(phase).toBe("working");
    }
    for (const end of ["done", "error", "cancelled", "idle"]) {
      expect(nextPhase("working", state(end))).toBe("idle");
    }
  });

  test("other events and a missing event leave the phase alone", () => {
    expect(nextPhase("recording", { type: "audio_level", session_id: "s", rms: 0.1 })).toBe("recording");
    expect(nextPhase("working", null)).toBe("working");
  });
});

describe("wiring", () => {
  const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

  test("the hub's page list and the Rust --hub list name the same pages in the same order", () => {
    const tsx = read("../src/hub/App.tsx");
    const pages = [...tsx.matchAll(/\{ id: "([a-z]+)", title:/g)].map((m) => m[1]);
    const rust = read("../src-tauri/src/app.rs");
    const block = rust.slice(rust.indexOf("pub const HUB_PAGES"));
    const listed = [...block.slice(0, block.indexOf("];")).matchAll(/"([a-z]+)"/g)].map((m) => m[1]);
    expect(pages).toContain("notes");
    expect(listed).toEqual(pages);
  });

  test("every daemon call the notes page makes has a Tauri command behind it", () => {
    const daemon = read("../src/lib/daemon.ts");
    const commands = read("../src-tauri/src/commands.rs");
    const app = read("../src-tauri/src/app.rs");
    for (const name of ["list_notes", "delete_note", "start_dictation", "stop"]) {
      expect(daemon).toContain(`"${name}"`);
      expect(commands).toContain(`pub async fn ${name}(`);
      expect(app).toContain(`crate::commands::${name},`);
    }
  });
});
