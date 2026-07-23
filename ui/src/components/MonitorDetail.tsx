import { useEffect, useState } from "preact/hooks";
import { api } from "../api";
import type { Sample, Status, Uptime } from "../types";

const HISTORY_LIMIT = 100;

const WINDOWS = [
  { label: "24h", secs: 86400 },
  { label: "7d", secs: 604800 },
] as const;

interface MonitorDetailProps {
  monitorId: number;
}

/**
 * Forensic detail for a monitor: an uptime timeline bar (with a 24h/7d
 * window toggle) plus a reverse-chronological history list. Mounted only
 * while the parent card is expanded, so it fetches fresh data each time
 * it's opened.
 */
export function MonitorDetail({ monitorId }: MonitorDetailProps) {
  const [windowSecs, setWindowSecs] = useState<number>(WINDOWS[0].secs);
  const [uptime, setUptime] = useState<Uptime | null>(null);
  const [uptimeLoading, setUptimeLoading] = useState(true);
  const [uptimeError, setUptimeError] = useState<string | null>(null);
  const [history, setHistory] = useState<Sample[] | null>(null);
  const [historyLoading, setHistoryLoading] = useState(true);
  const [historyError, setHistoryError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setUptimeLoading(true);
    setUptimeError(null);
    api
      .getUptime(monitorId, windowSecs)
      .then((data) => {
        if (!cancelled) setUptime(data);
      })
      .catch((err) => {
        if (!cancelled) {
          setUptimeError(err instanceof Error ? err.message : String(err));
        }
      })
      .finally(() => {
        if (!cancelled) setUptimeLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [monitorId, windowSecs]);

  useEffect(() => {
    let cancelled = false;
    setHistoryLoading(true);
    setHistoryError(null);
    api
      .getHistory(monitorId, HISTORY_LIMIT)
      .then((data) => {
        if (!cancelled) setHistory(data);
      })
      .catch((err) => {
        if (!cancelled) {
          setHistoryError(err instanceof Error ? err.message : String(err));
        }
      })
      .finally(() => {
        if (!cancelled) setHistoryLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [monitorId]);

  return (
    <div class="monitor-detail">
      <section class="uptime-section">
        <div class="uptime-header">
          <span class="uptime-label">{uptime ? uptimeLabel(uptime) : "Uptime"}</span>
          <div class="window-toggle" role="group" aria-label="Uptime window">
            {WINDOWS.map((w) => (
              <button
                key={w.secs}
                type="button"
                class={`window-btn ${windowSecs === w.secs ? "window-btn-active" : ""}`}
                onClick={() => setWindowSecs(w.secs)}
              >
                {w.label}
              </button>
            ))}
          </div>
        </div>
        {uptimeError && (
          <div class="detail-error">Failed to load uptime: {uptimeError}</div>
        )}
        {!uptimeError && uptimeLoading && (
          <div class="detail-loading">Loading uptime…</div>
        )}
        {!uptimeError && !uptimeLoading && uptime && <UptimeBuckets uptime={uptime} />}
      </section>

      <section class="history-section">
        <div class="history-header">History</div>
        {historyError && (
          <div class="detail-error">Failed to load history: {historyError}</div>
        )}
        {!historyError && historyLoading && (
          <div class="detail-loading">Loading history…</div>
        )}
        {!historyError && !historyLoading && history && history.length === 0 && (
          <div class="detail-empty">No history yet.</div>
        )}
        {!historyError && !historyLoading && history && history.length > 0 && (
          <ul class="history-list">
            {history.map((s, i) => (
              <li key={`${s.at}-${i}`} class="history-item">
                <span class="history-time">{new Date(s.at * 1000).toLocaleString()}</span>
                <span class={`dot dot-${s.status}`} aria-hidden="true" />
                <span class="history-message">{s.message || "(no message)"}</span>
                {s.components.length > 0 && (
                  <span class="history-components">
                    {s.components.length} component{s.components.length === 1 ? "" : "s"}
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

/** Number of discrete buckets to render for a given uptime window. */
function bucketCountFor(windowSecs: number): number {
  return windowSecs >= 604800 ? 84 : 48;
}

/** Worst-status ranking used to pick a bucket's color (higher wins). */
const STATUS_RANK: Record<Status, number> = {
  unknown: 0,
  ok: 1,
  degraded: 2,
  critical: 3,
};

function uptimeLabel(uptime: Uptime): string {
  const observed = uptime.ok_secs + uptime.degraded_secs + uptime.critical_secs;
  if (observed <= 0) return "no data yet";
  return `${uptime.percent_ok.toFixed(1)}% uptime`;
}

interface Bucket {
  start: number;
  end: number;
  status: Status | null;
}

function buildBuckets(uptime: Uptime): Bucket[] {
  const nowSecs = Math.floor(Date.now() / 1000);
  const segments = uptime.segments;
  const rangeStart = segments[0]?.start ?? nowSecs - uptime.window_secs;
  const rangeEnd = segments[segments.length - 1]?.end ?? nowSecs;
  const n = bucketCountFor(uptime.window_secs);
  const bucketSize = (rangeEnd - rangeStart) / n;

  const buckets: Bucket[] = [];
  for (let i = 0; i < n; i++) {
    const bStart = rangeStart + i * bucketSize;
    const bEnd = rangeStart + (i + 1) * bucketSize;
    let worst: Status | null = null;
    for (const seg of segments) {
      if (seg.start < bEnd && seg.end > bStart) {
        if (worst === null || STATUS_RANK[seg.status] > STATUS_RANK[worst]) {
          worst = seg.status;
        }
      }
    }
    buckets.push({ start: bStart, end: bEnd, status: worst });
  }
  return buckets;
}

function UptimeBuckets({ uptime }: { uptime: Uptime }) {
  if (uptime.segments.length === 0) {
    return <div class="detail-empty">No data in this window.</div>;
  }
  const buckets = buildBuckets(uptime);
  return (
    <div class="uptime-bucket-row">
      {buckets.map((b, i) => {
        const status = b.status ?? "unknown";
        const start = new Date(b.start * 1000).toLocaleString();
        const end = new Date(b.end * 1000).toLocaleString();
        return (
          <span
            key={i}
            class={`uptime-bucket uptime-bucket-${status}`}
            title={`${start} – ${end} · ${status}`}
          />
        );
      })}
    </div>
  );
}
