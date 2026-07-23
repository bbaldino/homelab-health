import type { MonitorStatus, Status } from "../types";
import { MonitorCard } from "./MonitorCard";

// Worst-first: critical > degraded > unknown/null > ok.
const RANK: Record<Status, number> = {
  critical: 0,
  degraded: 1,
  unknown: 2,
  ok: 3,
};

function rankOf(status: Status | null): number {
  return RANK[status ?? "unknown"];
}

interface StatusBoardProps {
  monitors: MonitorStatus[];
  onEdit: (monitor: MonitorStatus) => void;
  onDelete: (monitor: MonitorStatus) => void;
  onRunNow: (monitor: MonitorStatus) => Promise<void>;
}

export function StatusBoard({ monitors, onEdit, onDelete, onRunNow }: StatusBoardProps) {
  const sorted = [...monitors].sort((a, b) => rankOf(a.status) - rankOf(b.status));

  if (sorted.length === 0) {
    return null;
  }

  return (
    <div class="status-board">
      {sorted.map((monitor) => (
        <MonitorCard
          key={monitor.id}
          monitor={monitor}
          onEdit={onEdit}
          onDelete={onDelete}
          onRunNow={onRunNow}
        />
      ))}
    </div>
  );
}
