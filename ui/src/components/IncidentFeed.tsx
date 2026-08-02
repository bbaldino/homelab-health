import { useEffect, useState } from "preact/hooks";
import { api } from "../api";
import type { IncidentDetail } from "../types";
import { IncidentRow } from "./IncidentRow";

const LIMIT = 50;

const WINDOWS = [
  { label: "24h", secs: 86400 },
  { label: "7d", secs: 604800 },
] as const;

/**
 * Every monitor's incidents in one reverse-chronological list — the
 * "what happened overnight?" read, which the per-card badges can only hint
 * at. Mounted only while the board's toggle is on, so it fetches fresh data
 * each time it's opened.
 */
export function IncidentFeed() {
  const [windowSecs, setWindowSecs] = useState<number>(WINDOWS[0].secs);
  const [incidents, setIncidents] = useState<IncidentDetail[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .getIncidents({
        since: Math.floor(Date.now() / 1000) - windowSecs,
        limit: LIMIT,
      })
      .then((data) => {
        if (!cancelled) setIncidents(data);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [windowSecs]);

  return (
    <section class="incident-feed">
      <div class="incident-feed-header">
        <span class="uptime-label">Recent incidents</span>
        <div class="window-toggle" role="group" aria-label="Incident window">
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
      {error && <div class="detail-error">Failed to load incidents: {error}</div>}
      {!error && loading && <div class="detail-loading">Loading incidents…</div>}
      {!error && !loading && incidents && incidents.length === 0 && (
        <div class="detail-empty">
          No incidents in this window — every monitor stayed healthy.
        </div>
      )}
      {!error && !loading && incidents && incidents.length > 0 && (
        <ul class="incident-list">
          {incidents.map((incident) => (
            <IncidentRow
              key={`${incident.monitor_id}-${incident.started_at}`}
              incident={incident}
              showMonitor
            />
          ))}
        </ul>
      )}
    </section>
  );
}
