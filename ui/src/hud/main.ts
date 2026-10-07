// The Flow bar's view. No framework: it must paint within a frame of the
// window being mapped, and it has four moving parts.

import { daemon, onConnection, onDaemonEvent } from "../lib/daemon";
import { METER_BARS, formatElapsed, initialView, label, onConnection as connectionChanged, reduce, type HudView } from "../lib/hudModel";

const pill = document.getElementById("pill") as HTMLElement;
const meter = pill.querySelector(".meter") as HTMLElement;
const text = pill.querySelector(".label") as HTMLElement;
const timer = pill.querySelector(".timer") as HTMLElement;

const bars: HTMLElement[] = Array.from({ length: METER_BARS }, () => {
  const bar = document.createElement("i");
  meter.appendChild(bar);
  return bar;
});

let view: HudView = initialView();
let ticking: number | null = null;

function render(): void {
  pill.dataset["phase"] = view.phase;
  text.textContent = label(view);
  // A floor keeps silence visible as a row of dots rather than nothing.
  view.levels.forEach((level, i) => {
    const bar = bars[i];
    if (bar) bar.style.transform = `scaleY(${Math.max(0.09, level).toFixed(3)})`;
  });
  renderTimer();
  if (view.phase === "recording" && ticking === null) {
    ticking = window.setInterval(renderTimer, 250);
  } else if (view.phase !== "recording" && ticking !== null) {
    window.clearInterval(ticking);
    ticking = null;
  }
}

function renderTimer(): void {
  timer.textContent = view.startedAt === null ? "" : formatElapsed(performance.now() - view.startedAt);
}

void onDaemonEvent((event) => {
  view = reduce(view, event, performance.now());
  render();
});
void onConnection((connection) => {
  view = connectionChanged(view, connection);
  render();
});

// Catch up if this view loaded after a session began.
void daemon
  .lastSessionEvent()
  .then((event) => {
    if (event && view.phase === "hidden") {
      view = reduce(view, event, performance.now());
      render();
    }
  })
  .catch(() => {});

render();
