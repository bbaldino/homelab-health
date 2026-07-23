import { useEffect, useState } from "preact/hooks";
import { api } from "./api";
import type { MonitorStatus, Status } from "./types";
import { StatusBoard } from "./components/StatusBoard";

const POLL_INTERVAL_MS = 10_000;

interface Counts {
  ok: number;
  degraded: number;
  critical: number;
  unknown: number;
}

function countByStatus(monitors: MonitorStatus[]): Counts {
  const counts: Counts = { ok: 0, degraded: 0, critical: 0, unknown: 0 };
  for (const m of monitors) {
    const status: Status = m.status ?? "unknown";
    counts[status]++;
  }
  return counts;
}

export function App() {
  const [monitors, setMonitors] = useState<MonitorStatus[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null);
  const [loading, setLoading] = useState(true);

  async function refresh() {
    try {
      const data = await api.getStatus();
      setMonitors(data);
      setLastUpdated(new Date());
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, POLL_INTERVAL_MS);
    return () => clearInterval(id);
  }, []);

  const counts = countByStatus(monitors);

  return (
    <div class="app">
      <header class="app-header">
        <div class="app-header-top">
          <h1>Homelab Health</h1>
          <span class="refresh-hint" title="Status is polled automatically">
            auto-refreshing every 10s
          </span>
        </div>
        <div class="summary">
          <span class="summary-item summary-ok">
            <span class="dot dot-ok" /> {counts.ok} ok
          </span>
          <span class="summary-item summary-degraded">
            <span class="dot dot-degraded" /> {counts.degraded} degraded
          </span>
          <span class="summary-item summary-critical">
            <span class="dot dot-critical" /> {counts.critical} critical
          </span>
          <span class="summary-item summary-unknown">
            <span class="dot dot-unknown" /> {counts.unknown} unknown
          </span>
          {lastUpdated && (
            <span class="last-updated">
              updated {lastUpdated.toLocaleTimeString()}
            </span>
          )}
        </div>
      </header>

      <main>
        {error && <div class="error-banner">Failed to load status: {error}</div>}
        {loading && monitors.length === 0 && !error && (
          <div class="empty-state">Loading…</div>
        )}
        {!loading && monitors.length === 0 && !error && (
          <div class="empty-state">
            No monitors configured yet.
          </div>
        )}
        <StatusBoard monitors={monitors} />
      </main>
    </div>
  );
}
