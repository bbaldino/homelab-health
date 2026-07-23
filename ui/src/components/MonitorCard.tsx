import { useState } from "preact/hooks";
import type { MonitorStatus, Status } from "../types";

function statusLabel(status: Status | null): string {
  return status ?? "unknown";
}

interface MonitorCardProps {
  monitor: MonitorStatus;
}

export function MonitorCard({ monitor }: MonitorCardProps) {
  const [expanded, setExpanded] = useState(false);
  const hasComponents = monitor.components.length > 0;
  const status = statusLabel(monitor.status);

  return (
    <div class={`monitor-card status-${status}`}>
      <button
        type="button"
        class="monitor-card-header"
        onClick={() => hasComponents && setExpanded((v) => !v)}
        aria-expanded={hasComponents ? expanded : undefined}
        disabled={!hasComponents}
      >
        <span class={`dot dot-${status}`} aria-hidden="true" />
        <span class="monitor-name">{monitor.name}</span>
        <span class="monitor-type">{monitor.type_id}</span>
        <span class="monitor-message">
          {monitor.message ?? (monitor.status === null ? "not yet checked" : "")}
        </span>
        {hasComponents && (
          <span class={`chevron ${expanded ? "chevron-open" : ""}`} aria-hidden="true">
            ▸
          </span>
        )}
        {!monitor.enabled && <span class="disabled-badge">disabled</span>}
      </button>

      {hasComponents && (
        <div class={`components ${expanded ? "components-open" : ""}`}>
          <div class="components-inner">
            <ul class="component-list">
              {monitor.components.map((c) => (
                <li key={c.name} class="component-item">
                  <span class={`dot dot-${c.status}`} aria-hidden="true" />
                  <span class="component-name">{c.name}</span>
                  {c.critical && <span class="critical-marker">critical</span>}
                  {c.message && <span class="component-message">{c.message}</span>}
                </li>
              ))}
            </ul>
          </div>
        </div>
      )}
    </div>
  );
}
