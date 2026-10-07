import { useState } from "preact/hooks";
import { daemon } from "../../lib/daemon";
import { formatCount, formatWpm, recentDays, relativeTime, storedState, wordsThisWeek } from "../../lib/format";
import type { HistoryAnalytics, HistoryEntry, Status } from "../../lib/protocol";
import { useHub, useLoad } from "../useDaemon";

export function Home() {
  const { refresh, connection } = useHub();
  const live = connection.status === "connected";
  const analytics = useLoad(() => (live ? daemon.analytics() : Promise.resolve(null)), [refresh, live]);
  const recent = useLoad(() => (live ? daemon.queryHistory({ limit: 5 }) : Promise.resolve(null)), [refresh, live]);
  const status = useLoad(() => (live ? daemon.getStatus() : Promise.resolve(null)), [refresh, live]);
  const now = new Date();

  return (
    <div class="page">
      <header class="page-head">
        <h1>Home</h1>
      </header>
      <FormatterWarning status={status.data} />
      {analytics.error && <p class="error">Could not load analytics: {analytics.error}</p>}
      <section class="tiles" aria-label="Dictation totals">
        <Tile label="Words per minute" value={formatWpm(analytics.data?.overall_wpm)} />
        <Tile label="Words today" value={analytics.data ? formatCount(analytics.data.words_today) : "—"} />
        <Tile label="Words this week" value={analytics.data ? formatCount(wordsThisWeek(analytics.data, now)) : "—"} />
        <Tile
          label="Streak"
          value={analytics.data ? `${analytics.data.current_streak_days} d` : "—"}
          note={analytics.data ? `longest ${analytics.data.longest_streak_days} d` : undefined}
        />
      </section>
      {analytics.data && <WordsChart analytics={analytics.data} now={now} />}
      <section class="card">
        <div class="card-head">
          <h2>Recent dictations</h2>
          <a href="#/history">All history</a>
        </div>
        <RecentList items={recent.data?.items ?? []} loading={recent.loading && live} />
      </section>
    </div>
  );
}

function Tile(props: { label: string; value: string; note?: string | undefined }) {
  return (
    <div class="tile">
      <div class="tile-label">{props.label}</div>
      <div class="tile-value">{props.value}</div>
      {props.note && <div class="tile-note muted">{props.note}</div>}
    </div>
  );
}

/** Silent degradation is a bug class here: say so when the formatter cannot run. */
function FormatterWarning({ status }: { status: Status | null }) {
  const f = status?.formatter;
  if (!f || !f.enabled || f.health === "ok" || f.health === "unchecked" || f.health === "disabled") return null;
  const what =
    f.health === "model_missing"
      ? `The formatter model${f.model ? ` “${f.model}”` : ""} is not installed, so the LLM polish step is being skipped.`
      : f.health === "unreachable"
        ? "The formatter (Ollama) is unreachable, so the LLM polish step is being skipped."
        : "The formatter is failing, so dictations are delivered without the LLM polish step.";
  return (
    <section class="banner warn" role="status">
      <div>
        <strong>Formatting is degraded.</strong> {what}
        {f.detail && <div class="muted small">{f.detail}</div>}
      </div>
      <a class="button" href="#/doctor">
        Open Doctor
      </a>
    </section>
  );
}

const CHART_DAYS = 14;

function WordsChart({ analytics, now }: { analytics: HistoryAnalytics; now: Date }) {
  const days = recentDays(analytics, now, CHART_DAYS);
  const max = Math.max(1, ...days.map((d) => d.words));
  const [active, setActive] = useState<number | null>(null);
  const W = 560;
  const H = 140;
  const pad = { top: 18, bottom: 20 };
  const plotH = H - pad.top - pad.bottom;
  const slot = W / CHART_DAYS;
  const barW = slot - 2; // 2px surface gap between bars
  const peak = days.reduce((best, d, i) => (d.words > (days[best]?.words ?? 0) ? i : best), 0);
  const shown = active ?? null;

  return (
    <section class="card chart-card">
      <div class="card-head">
        <h2>Words per day, last {CHART_DAYS} days</h2>
      </div>
      <div class="chart" onMouseLeave={() => setActive(null)}>
        <svg viewBox={`0 0 ${W} ${H}`} role="img" aria-label={`Words per day for the last ${CHART_DAYS} days`}>
          <line class="baseline" x1="0" x2={W} y1={H - pad.bottom} y2={H - pad.bottom} />
          {days.map((d, i) => {
            const h = d.words === 0 ? 0 : Math.max(2, (d.words / max) * plotH);
            const x = i * slot + 1;
            const y = H - pad.bottom - h;
            const r = Math.min(4, h / 2, barW / 2);
            return (
              <g
                key={d.day}
                class={`bar-group${shown === i ? " active" : ""}`}
                tabIndex={0}
                aria-label={`${d.day}: ${formatCount(d.words)} words`}
                onMouseEnter={() => setActive(i)}
                onFocus={() => setActive(i)}
                onBlur={() => setActive(null)}
              >
                {/* Hit target: the whole column, bigger than the mark. */}
                <rect class="hit" x={i * slot} y={0} width={slot} height={H - pad.bottom} />
                {h > 0 && <path class="bar" d={roundedTop(x, y, barW, h, r)} />}
                {(i === peak && d.words > 0) || i === CHART_DAYS - 1 ? (
                  <text class="axis-label" x={x + barW / 2} y={H - 6} text-anchor="middle">
                    {i === CHART_DAYS - 1 ? "today" : d.day.slice(5)}
                  </text>
                ) : null}
                {i === peak && d.words > 0 && (
                  <text class="value-label" x={x + barW / 2} y={y - 5} text-anchor="middle">
                    {formatCount(d.words)}
                  </text>
                )}
              </g>
            );
          })}
        </svg>
        {shown !== null && days[shown] && (
          <div class="tooltip" style={{ left: `${((shown + 0.5) / CHART_DAYS) * 100}%` }} role="status">
            <strong>{formatCount(days[shown].words)}</strong> words
            <div class="muted small">{days[shown].day}</div>
          </div>
        )}
      </div>
      <details class="table-view">
        <summary>Show as table</summary>
        <table>
          <thead>
            <tr>
              <th scope="col">Day (UTC)</th>
              <th scope="col">Words</th>
            </tr>
          </thead>
          <tbody>
            {days.map((d) => (
              <tr key={d.day}>
                <td>{d.day}</td>
                <td class="num">{formatCount(d.words)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </details>
    </section>
  );
}

/** A bar with rounded top corners and a square base on the baseline. */
function roundedTop(x: number, y: number, w: number, h: number, r: number): string {
  return `M${x},${y + h} V${y + r} Q${x},${y} ${x + r},${y} H${x + w - r} Q${x + w},${y} ${x + w},${y + r} V${y + h} Z`;
}

function RecentList({ items, loading }: { items: HistoryEntry[]; loading: boolean }) {
  if (loading && items.length === 0) return <p class="muted">Loading…</p>;
  if (items.length === 0) return <p class="muted">No dictations yet. Press your dictation key and start talking.</p>;
  const now = Date.now();
  return (
    <ul class="recent">
      {items.map((e) => (
        <li key={e.id}>
          <span class="when muted">{relativeTime(e.ts_ms, now)}</span>
          <span class="text">
            {storedState(e) === "stored" ? (
              e.text
            ) : storedState(e) === "private" ? (
              <em class="muted">Not stored (privacy mode)</em>
            ) : (
              <em class="error-text">{e.error?.message ?? "Failed"}</em>
            )}
          </span>
        </li>
      ))}
    </ul>
  );
}
