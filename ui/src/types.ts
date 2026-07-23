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
 * A monitor plus its latest known status. `status` is null until the first
 * check has run for this monitor.
 */
export interface MonitorStatus extends Monitor {
  status: Status | null;
  message: string | null;
  components: Component[];
  updated_at: string | null;
}

/** One field in a check type's config schema. */
export interface Field {
  name: string;
  kind: "string" | "int" | "float" | "bool";
  required: boolean;
  default: unknown;
  help: string;
  secret: boolean;
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
