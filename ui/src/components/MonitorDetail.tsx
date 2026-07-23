import { useEffect, useState } from "preact/hooks";
import { api } from "../api";
import type { Sample, Uptime } from "../types";

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
          <span class="uptime-label">
            {uptime ? `${uptime.percent_ok.toFixed(1)}% uptime` : "Uptime"}
          </span>
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
        {!uptimeError && !uptimeLoading && uptime && <UptimeBar uptime={uptime} />}
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

function UptimeBar({ uptime }: { uptime: Uptime }) {
  if (uptime.segments.length === 0) {
    return <div class="detail-empty">No data in this window.</div>;
  }
  return (
    <div class="uptime-bar">
      {uptime.segments.map((seg, i) => {
        const widthPct = Math.max(
          0,
          ((seg.end - seg.start) / uptime.window_secs) * 100,
        );
        const start = new Date(seg.start * 1000).toLocaleString();
        const end = new Date(seg.end * 1000).toLocaleString();
        return (
          <span
            key={i}
            class={`uptime-segment uptime-segment-${seg.status}`}
            style={{ width: `${widthPct}%` }}
            title={`${seg.status} · ${start} – ${end}`}
          />
        );
      })}
    </div>
  );
}
