import { useState } from "preact/hooks";
import type { IncidentDetail } from "../types";

/**
 * Compact human duration: "45s", "29m", "1h 12m", "2d 3h". Incidents are
 * measured in seconds but read in minutes and hours, and a row has no space
 * for more than two units.
 */
export function formatDuration(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) {
    const remMins = mins % 60;
    return remMins === 0 ? `${hours}h` : `${hours}h ${remMins}m`;
  }
  const days = Math.floor(hours / 24);
  const remHours = hours % 24;
  return remHours === 0 ? `${days}d` : `${days}d ${remHours}h`;
}

interface IncidentRowProps {
  incident: IncidentDetail;
  /** Show which monitor this belongs to — for the cross-monitor feed. */
  showMonitor?: boolean;
}

/**
 * One incident, collapsed to a single line and expandable to the components
 * that failed during it. Shared by the per-monitor detail list and the
 * cross-monitor feed, which differ only in whether the monitor is named.
 */
export function IncidentRow({ incident, showMonitor = false }: IncidentRowProps) {
  const [expanded, setExpanded] = useState(false);
  const ongoing = incident.ended_at === null;
  const started = new Date(incident.started_at * 1000);
  const duration = formatDuration(incident.duration_secs);

  return (
    <li class="incident-item">
      <button
        type="button"
        class="incident-summary"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
      >
        <span class={`chevron ${expanded ? "chevron-open" : ""}`} aria-hidden="true">
          ▸
        </span>
        <span class={`dot dot-${incident.worst_status}`} aria-hidden="true" />
        <span class="incident-time" title={incidentRangeTitle(incident)}>
          {started.toLocaleString()}
        </span>
        {showMonitor && <span class="incident-monitor">{incident.monitor_name}</span>}
        <span class={`incident-duration ${ongoing ? "incident-ongoing" : ""}`}>
          {ongoing ? `ongoing · ${duration}` : duration}
        </span>
        <span class="incident-message">{incident.message || "(no message)"}</span>
      </button>

      {expanded && (
        <div class="incident-components">
          {incident.failing_components.length === 0 ? (
            <div class="detail-empty">
              No component detail — this check reports no components, or its
              samples have aged out of the 7-day retention window.
            </div>
          ) : (
            <ul class="failing-component-list">
              {incident.failing_components.map((c) => (
                <li key={c.name} class="failing-component-item">
                  <span class={`dot dot-${c.worst_status}`} aria-hidden="true" />
                  <span class="component-name">{c.name}</span>
                  {c.critical && <span class="critical-marker">critical</span>}
                  <span class="component-message">{c.message || "(no message)"}</span>
                  <span class="failing-component-seen" title="first – last seen failing">
                    {new Date(c.first_seen * 1000).toLocaleTimeString()} –{" "}
                    {new Date(c.last_seen * 1000).toLocaleTimeString()}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </li>
  );
}

/** Tooltip spelling out the full span, since the row shows only the start. */
function incidentRangeTitle(incident: IncidentDetail): string {
  const started = new Date(incident.started_at * 1000).toLocaleString();
  if (incident.ended_at === null) return `${started} – ongoing`;
  return `${started} – ${new Date(incident.ended_at * 1000).toLocaleString()}`;
}
