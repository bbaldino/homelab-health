import type {
  CheckReport,
  CheckTypeSchema,
  Monitor,
  MonitorStatus,
  NewMonitor,
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
}

export const api = new ApiClient();
