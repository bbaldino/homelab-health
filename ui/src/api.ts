import type {
  CheckReport,
  CheckTypeSchema,
  IncidentDetail,
  Monitor,
  MonitorStatus,
  NewMonitor,
  PrometheusInspect,
  Sample,
  Uptime,
} from "./types";

/** Thrown when the backend responds with a non-2xx status. */
export class ApiError extends Error {
  status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`/api/v1${path}`, {
    headers: init?.body ? { "Content-Type": "application/json" } : undefined,
    ...init,
  });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new ApiError(
      res.status,
      `${init?.method ?? "GET"} ${path} failed: ${res.status} ${res.statusText}${body ? ` — ${body}` : ""}`,
    );
  }
  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}

/** Filters for GET /incidents; `since`/`until` are unix epoch-seconds. */
export interface IncidentQuery {
  since?: number;
  until?: number;
  monitorId?: number;
  limit?: number;
}

/** Typed client for the homelab-health JSON API. */
export class ApiClient {
  getStatus(): Promise<MonitorStatus[]> {
    return request<MonitorStatus[]>("/status");
  }

  getCheckTypes(): Promise<CheckTypeSchema[]> {
    return request<CheckTypeSchema[]>("/check-types");
  }

  createMonitor(m: NewMonitor): Promise<Monitor> {
    return request<Monitor>("/monitors", {
      method: "POST",
      body: JSON.stringify(m),
    });
  }

  updateMonitor(id: number, m: NewMonitor): Promise<Monitor> {
    return request<Monitor>(`/monitors/${id}`, {
      method: "PUT",
      body: JSON.stringify(m),
    });
  }

  deleteMonitor(id: number): Promise<void> {
    return request<void>(`/monitors/${id}`, { method: "DELETE" });
  }

  runNow(id: number): Promise<CheckReport> {
    return request<CheckReport>(`/monitors/${id}/run`, { method: "POST" });
  }

  getHistory(id: number, limit?: number): Promise<Sample[]> {
    const query = limit !== undefined ? `?limit=${limit}` : "";
    return request<Sample[]>(`/monitors/${id}/history${query}`);
  }

  getUptime(id: number, windowSecs: number): Promise<Uptime> {
    return request<Uptime>(`/monitors/${id}/uptime?window=${windowSecs}`);
  }

  /**
   * Incidents newest first. Every parameter is optional: the server defaults
   * to the last 7 days, all monitors, and a limit of 50.
   */
  getIncidents(params: IncidentQuery = {}): Promise<IncidentDetail[]> {
    const query = new URLSearchParams();
    if (params.since !== undefined) query.set("since", String(params.since));
    if (params.until !== undefined) query.set("until", String(params.until));
    if (params.monitorId !== undefined) query.set("monitor_id", String(params.monitorId));
    if (params.limit !== undefined) query.set("limit", String(params.limit));
    const qs = query.toString();
    return request<IncidentDetail[]>(`/incidents${qs ? `?${qs}` : ""}`);
  }

  inspectPrometheus(url: string, timeoutSecs?: number): Promise<PrometheusInspect> {
    return request<PrometheusInspect>("/checks/prometheus/inspect", {
      method: "POST",
      body: JSON.stringify({ url, timeout_secs: timeoutSecs }),
    });
  }
}

export const api = new ApiClient();
