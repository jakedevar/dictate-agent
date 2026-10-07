// What the Flow bar shows, as a pure reducer over daemon events. The Rust
// side decides when the window is mapped (src-tauri/src/hud.rs); this decides
// what is drawn in it. Kept free of the DOM so it is unit-tested directly.

import type { Connection, DaemonEvent } from "./protocol";

export type HudPhase =
  | "hidden"
  | "recording"
  | "transcribing"
  | "formatting"
  | "injecting"
  | "done"
  | "error"
  | "cancelled";

/** Bars in the level meter. */
export const METER_BARS = 24;

export interface HudView {
  phase: HudPhase;
  sessionId: string | null;
  /** When recording started (ms, caller's clock). */
  startedAt: number | null;
  /** Normalized levels, oldest first, `METER_BARS` long. */
  levels: number[];
  /** Words delivered, once the final transcript arrives. */
  words: number | null;
  /** A short, human message for the error phase. */
  message: string | null;
}

export function initialView(): HudView {
  return {
    phase: "hidden",
    sessionId: null,
    startedAt: null,
    levels: new Array<number>(METER_BARS).fill(0),
    words: null,
    message: null,
  };
}

/**
 * Map RMS (0..1) to a bar height (0..1). Speech sits around 0.02–0.2 RMS, so a
 * linear scale would leave the meter nearly flat; a square-root curve against a
 * 0.25 ceiling makes ordinary speech fill most of the bar without clipping.
 */
export function normalizeLevel(rms: number): number {
  if (!Number.isFinite(rms) || rms <= 0) return 0;
  return Math.min(1, Math.sqrt(rms / 0.25));
}

const ACTIVE: ReadonlySet<string> = new Set(["recording", "transcribing", "formatting", "injecting"]);
const TERMINAL: ReadonlySet<string> = new Set(["done", "error", "cancelled"]);

export function reduce(view: HudView, event: DaemonEvent, now: number): HudView {
  switch (event.type) {
    case "state_changed": {
      const to = String(event["to"]);
      const session = String(event["session_id"] ?? "");
      if (to === "recording") {
        return { ...initialView(), phase: "recording", sessionId: session, startedAt: now };
      }
      if (ACTIVE.has(to)) {
        // An upload (dictate transcribe) starts at transcribing: no meter.
        const fresh = view.sessionId !== session;
        return {
          ...(fresh ? initialView() : view),
          phase: to as HudPhase,
          sessionId: session,
        };
      }
      if (TERMINAL.has(to)) {
        if (view.phase === "hidden") return view; // nothing was on screen
        return { ...view, phase: to as HudPhase };
      }
      if (to === "idle" && TERMINAL.has(view.phase)) return view; // let it linger
      return view;
    }
    case "audio_level": {
      if (view.phase !== "recording") return view;
      const rms = typeof event["rms"] === "number" ? event["rms"] : 0;
      return { ...view, levels: [...view.levels.slice(1), normalizeLevel(rms)] };
    }
    case "final": {
      const words = event["word_count"];
      return { ...view, words: typeof words === "number" ? words : view.words };
    }
    case "error": {
      const error = event["error"] as { message?: unknown; code?: unknown } | undefined;
      const message = typeof error?.message === "string" ? error.message : String(error?.code ?? "error");
      return { ...view, message: shorten(message, 38) };
    }
    default:
      return view;
  }
}

/** Losing the daemon ends whatever was on screen. */
export function onConnection(view: HudView, connection: Connection): HudView {
  return connection.status === "connected" ? view : initialView();
}

export function shorten(text: string, max: number): string {
  const clean = text.replace(/\s+/g, " ").trim();
  return clean.length <= max ? clean : `${clean.slice(0, max - 1).trimEnd()}…`;
}

/** `m:ss` for the recording timer. */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

/** The label under each phase. */
export function label(view: HudView): string {
  switch (view.phase) {
    case "recording":
      return "Listening";
    case "transcribing":
      return "Transcribing";
    case "formatting":
      return "Polishing";
    case "injecting":
      return "Pasting";
    case "done":
      return view.words === null ? "Done" : `${view.words} ${view.words === 1 ? "word" : "words"}`;
    case "error":
      return view.message ?? "Something went wrong";
    case "cancelled":
      return "Cancelled";
    case "hidden":
      return "";
  }
}
