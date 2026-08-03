// Mirrors the JSON contracts served by the Rust backend under /api/v1/*.

/** Severity/health status of a monitor or component. */
export type Status = "ok" | "degraded" | "critical" | "unknown";

/** A single sub-check inside a monitor's report (e.g. one camera in a Frigate check). */
export interface Component {
  name: string;
  status: Status;
  critical: boolean;
  message: string;
}

/** A configured monitor, as stored/returned by the backend. */
export interface Monitor {
  id: number;
  name: string;
  type_id: string;
  config: Record<string, unknown>;
  interval_secs: number;
  enabled: boolean;
}

/** Payload for creating or updating a monitor (no id). */
export interface NewMonitor {
  name: string;
  type_id: string;
  config: Record<string, unknown>;
  interval_secs: number;
  enabled: boolean;
}

/**
 * A bounded period during which a monitor was not Ok. Timestamps are unix
 * epoch-seconds. `ended_at` is null while the incident is still ongoing —
 * there is deliberately no separate "resolved" flag.
 */
export interface Incident {
  started_at: number;
  ended_at: number | null;
  duration_secs: number;
  worst_status: Status;
  message: string;
}

/**
 * A component that was non-Ok at some point during an incident, folded over
 * the incident's whole lifetime rather than snapshotted from one sample.
 * Healthy components during an outage are omitted.
 */
export interface FailingComponent {
  name: string;
  worst_status: Status;
  critical: boolean;
  message: string;
  first_seen: number;
  last_seen: number;
}

/**
 * An incident as returned by GET /incidents: identified by monitor and
 * carrying the per-component detail that the inline /status shape omits.
 * `failing_components` is empty for checks that emit no components (http,
 * tcp) and for incidents whose samples have aged out of the 7-day prune.
 */
export interface IncidentDetail extends Incident {
  monitor_id: number;
  monitor_name: string;
  failing_components: FailingComponent[];
}

/**
 * A monitor plus its latest known status. `status` is null until the first
 * check has run for this monitor. `recent_incidents` covers the last 24h,
 * newest 5, without per-component detail.
 */
export interface MonitorStatus extends Monitor {
  status: Status | null;
  message: string | null;
  components: Component[];
  updated_at: string | null;
  recent_incidents: Incident[];
}

/** One field in a check type's config schema. */
export interface Field {
  name: string;
  kind: "string" | "int" | "float" | "bool" | "list";
  required: boolean;
  default: unknown;
  help: string;
  secret: boolean;
  options?: (string | number | boolean)[];
  fields?: Field[];
}

/** The config schema advertised by a check type (e.g. "http", "tcp"). */
export interface CheckTypeSchema {
  type_id: string;
  schema: { fields: Field[] };
}

/** Result of running a check immediately (POST /monitors/:id/run). */
export interface CheckReport {
  status: Status;
  message: string;
  components: Component[];
}

/**
 * A single recorded check result for a monitor (history entry). `at` is a
 * unix epoch-seconds timestamp — render via `new Date(at * 1000)`.
 */
export interface Sample {
  status: Status;
  message: string;
  components: Component[];
  at: number;
}

/** One contiguous run of a single status within an uptime window. */
export interface Segment {
  status: Status;
  start: number;
  end: number;
}

/** Uptime summary for a monitor over a trailing window (GET .../uptime). */
export interface Uptime {
  window_secs: number;
  ok_secs: number;
  degraded_secs: number;
  critical_secs: number;
  unknown_secs: number;
  percent_ok: number;
  segments: Segment[];
}

/** Result of inspecting a Prometheus metrics endpoint (POST .../checks/prometheus/inspect). */
export interface PrometheusInspect {
  metrics: Record<string, { labels: Record<string, string[]> }>;
}
