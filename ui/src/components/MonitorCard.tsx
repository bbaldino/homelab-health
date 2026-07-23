import { useEffect, useRef, useState } from "preact/hooks";
import type { MonitorStatus, Status } from "../types";
import { MonitorDetail } from "./MonitorDetail";

function statusLabel(status: Status | null): string {
  return status ?? "unknown";
}

interface MonitorCardProps {
  monitor: MonitorStatus;
  onEdit: (monitor: MonitorStatus) => void;
  onDelete: (monitor: MonitorStatus) => void;
  onRunNow: (monitor: MonitorStatus) => Promise<void>;
}

export function MonitorCard({ monitor, onEdit, onDelete, onRunNow }: MonitorCardProps) {
  const [expanded, setExpanded] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [menuPos, setMenuPos] = useState<{ top: number; right: number } | null>(null);
  const [running, setRunning] = useState(false);
  const actionsRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const hasComponents = monitor.components.length > 0;
  const status = statusLabel(monitor.status);

  // The menu is rendered with position:fixed (anchored to the trigger's
  // viewport rect) rather than position:absolute, so it isn't clipped by
  // .monitor-card's `overflow: hidden` (used for the rounded corners /
  // expand-collapse animation).
  function toggleMenu() {
    if (!menuOpen && triggerRef.current) {
      const rect = triggerRef.current.getBoundingClientRect();
      setMenuPos({ top: rect.bottom + 4, right: window.innerWidth - rect.right });
    }
    setMenuOpen((v) => !v);
  }

  useEffect(() => {
    if (!menuOpen) return;
    function onDocMouseDown(e: MouseEvent) {
      if (actionsRef.current && !actionsRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    }
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") setMenuOpen(false);
    }
    document.addEventListener("mousedown", onDocMouseDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("mousedown", onDocMouseDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [menuOpen]);

  async function handleRunNow() {
    setMenuOpen(false);
    setRunning(true);
    try {
      await onRunNow(monitor);
    } catch (err) {
      alert(`Run failed: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setRunning(false);
    }
  }

  function handleDelete() {
    setMenuOpen(false);
    if (window.confirm(`Delete monitor "${monitor.name}"? This cannot be undone.`)) {
      onDelete(monitor);
    }
  }

  function handleEdit() {
    setMenuOpen(false);
    onEdit(monitor);
  }

  return (
    <div class={`monitor-card status-${status}`}>
      <div class="monitor-card-row">
        <button
          type="button"
          class="monitor-card-header"
          onClick={() => setExpanded((v) => !v)}
          aria-expanded={expanded}
        >
          <span class={`dot dot-${status}`} aria-hidden="true" />
          <span class="monitor-name">{monitor.name}</span>
          <span class="monitor-type">{monitor.type_id}</span>
          <span class="monitor-message">
            {monitor.message ?? (monitor.status === null ? "not yet checked" : "")}
          </span>
          <span class={`chevron ${expanded ? "chevron-open" : ""}`} aria-hidden="true">
            ▸
          </span>
          {!monitor.enabled && <span class="disabled-badge">disabled</span>}
        </button>

        <div class="monitor-card-actions" ref={actionsRef}>
          <button
            type="button"
            class="menu-trigger"
            aria-haspopup="true"
            aria-expanded={menuOpen}
            aria-label={`Actions for ${monitor.name}`}
            disabled={running}
            ref={triggerRef}
            onClick={toggleMenu}
          >
            {running ? "…" : "⋯"}
          </button>
          {menuOpen && menuPos && (
            <div
              class="action-menu"
              role="menu"
              style={{ top: `${menuPos.top}px`, right: `${menuPos.right}px` }}
            >
              <button type="button" role="menuitem" onClick={handleEdit}>
                Edit
              </button>
              <button type="button" role="menuitem" onClick={handleRunNow}>
                Run now
              </button>
              <button
                type="button"
                role="menuitem"
                class="action-menu-danger"
                onClick={handleDelete}
              >
                Delete
              </button>
            </div>
          )}
        </div>
      </div>

      <div class={`components ${expanded ? "components-open" : ""}`}>
        <div class="components-inner">
          {hasComponents && (
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
          )}
          {expanded && <MonitorDetail monitorId={monitor.id} />}
        </div>
      </div>
    </div>
  );
}
