# Homelab Health Monitor — Design

**Date:** 2026-07-22
**Status:** Approved (design), pending implementation plan

## Problem

Many services run across a homelab (infrastructure, media, home automation,
network). Each has a different notion of "healthy," and the operator wants to be
alerted when any service enters an unhealthy state. Health must always reduce to
a discrete status, but services can have sub-components with independent
severity (critical vs non-critical subsystems).

Off-the-shelf uptime tools (Uptime Kuma, Gatus) model monitors as binary
up/down and cannot natively express per-component detail or a distinct
"degraded" tier. Example: Frigate can be running while one of six cameras is
dark, or while recordings silently stop. We want first-class severity tiers,
per-component rollups, and custom per-service logic — so we are building a
small custom aggregator rather than fighting a binary-oriented tool.

## Goals

- Model health as `Ok / Degraded / Critical / Unknown`, per monitor and per
  sub-component, with critical vs non-critical rollup semantics.
- Make simple monitors (HTTP/TCP/ping/cert) fully configurable from the web UI
  with no code changes.
- Make genuinely new check *logic* a small, well-isolated plugin.
- Alert on state transitions via pluggable notifiers (Home Assistant, ntfy).
- Expose a JSON status API as a first-class output for other consumers.
- Ship as a single binary + SQLite file, deployable as one container on Unraid.

## Non-goals (v1)

- Built-in authentication (assumed to sit behind a reverse proxy / on-LAN).
- Frozen-frame / image-diff camera detection (high effort; revisit later).
- Metrics/time-series storage beyond recent status history.
- Plugins beyond the v1 set (Music Assistant, Unraid, Plex come later, same way).

## Core concepts

### Status

```
enum Status { Ok, Degraded, Critical, Unknown }
```

- `Ok` — healthy.
- `Degraded` — working but impaired (quality/perf loss, non-critical subsystem down).
- `Critical` — not functioning / operator must act.
- `Unknown` — the check itself could not run (config error, network failure,
  timeout). Distinct from the service being down.

### CheckReport

A single check run produces:

```
struct CheckReport {
    status: Status,
    message: String,          // human-readable "why"; REQUIRED when status != Ok
    components: Vec<Component>, // optional sub-components
}

struct Component {
    name: String,             // e.g. "driveway"
    status: Status,
    critical: bool,           // criticality of THIS component
    message: String,          // REQUIRED when status != Ok
}
```

`message` is the detail string surfaced in the API and UI (e.g.
`"driveway camera_fps=0 for 45s"`, `"cert expires in 3 days"`, `"HTTP 503"`).

### Rollup rules

When a report has components, the parent status is derived:

- A failing **critical** component propagates its severity to the parent
  (a `Critical` critical-component ⇒ parent `Critical`).
- A failing **non-critical** component caps the parent at **`Degraded`** — it
  can never push the parent to `Critical`.
- `Unknown` components are treated as non-critical-degrading unless the
  component is flagged critical, in which case they surface as `Unknown` at the
  parent (we don't know, and it matters).
- Parent status = worst status among components under the above rules.
- The parent `message` summarizes the driving component(s), e.g.
  `"Frigate: Critical — driveway down"`.

## Extensibility model (plugin vs instance)

Two distinct things, kept separate:

- **Check types (plugins) = code.** A `Check` implementation:

  ```
  trait Check {
      async fn run(&self, cfg: &Config) -> CheckReport;
      fn schema() -> ConfigSchema;   // fields for the UI form
  }
  ```

  Built-in v1 types: `http`, `tcp`, `ping`, `cert-expiry`, `frigate-camera`.
  `frigate-camera` is included specifically to prove the custom-plugin model
  end to end.

- **Monitor instances = data.** Rows in SQLite: name, check type id, config
  JSON, interval, enabled, notifier settings. Created / edited / deleted
  entirely from the web UI. No restart to add or change a monitor.

- **Registry** maps `type_id → factory + ConfigSchema`.

- **Schema-driven forms.** Each check type declares a `ConfigSchema` (field
  name, type, required, default, help text). The UI renders the add/edit form
  generically from that schema, so a newly-compiled plugin automatically has a
  working UI form. Writing code is required only for new *logic*, never for a
  new instance of an existing type.

## Execution & persistence

- **Scheduler.** Each enabled monitor runs on its own interval, concurrently,
  with a per-check timeout. A check that times out yields `Unknown` with an
  explanatory message.
- **Debounce / hysteresis.** A committed state change requires N consecutive
  matching results (configurable, small default). This prevents a flapping
  camera or transient blip from spamming alerts.
- **SQLite schema (initial):**
  - `monitors(id, name, type_id, config_json, interval_secs, enabled,
    notifier_json, created_at, updated_at)`
  - `status_current(monitor_id PK, status, message, components_json, updated_at)`
  - `status_history(id, monitor_id, status, message, components_json, at)` —
    append-only, records committed transitions; powers the UI timeline and
    debounce evaluation.

## Alerting

- **`Notifier` trait**, fired on committed state transitions (not on every
  check run).

  ```
  trait Notifier {
      async fn notify(&self, event: &TransitionEvent) -> Result<()>;
  }
  ```

- v1 impls:
  - **Home Assistant** — mobile push notification, and expose each monitor as a
    state entity (so HA dashboards/automations can consume it).
  - **ntfy** — simple topic push.
- Per-monitor config: which notifiers to use, and the minimum severity that
  triggers an alert.

## Web API (JSON)

- `GET /api/v1/status` — all monitors, current status + components + messages.
  The headline output for external consumers (HA, dashboards).
- `GET /api/v1/status/:id` — one monitor, with recent history.
- `GET /api/v1/monitors`, `POST`, `PUT /:id`, `DELETE /:id` — CRUD.
- `GET /api/v1/check-types` — available types and their config schemas (drives
  the UI form).
- `POST /api/v1/monitors/:id/run` — run a check on demand.

## Web UI (TypeScript)

- **Stack:** TypeScript + Vite + Preact, built to static assets served by the
  axum backend. Single binary + SQLite file; one container on Unraid.
- **Views:**
  - Dashboard: status tiles (Ok/Degraded/Critical/Unknown color-coded), each
    showing the monitor name and current message.
  - Detail: a monitor's components, current message, and history timeline.
  - Add/edit monitor: form generated from the selected check type's
    `ConfigSchema`; set interval, enabled, notifiers, min severity.

## Architecture summary

```
+-------------------+      +------------------+
|  Web UI (Preact,  | <--> |  axum HTTP layer |
|  TS, static)      |      |  - JSON API      |
+-------------------+      |  - static assets |
                          +---------+--------+
                                    |
                 +------------------+------------------+
                 |                  |                  |
          +------v-----+   +--------v-------+   +------v------+
          | Scheduler  |   | Check registry |   |  Notifiers  |
          | (intervals,|   | http/tcp/ping/ |   |  HA / ntfy  |
          |  timeouts, |   | cert/frigate   |   +------+------+
          |  debounce) |   +--------+-------+          |
          +------+-----+            |                  |
                 |          runs Check::run     on transition
                 v                  |                  |
          +------+------------------v------------------+------+
          |                     SQLite                        |
          |  monitors / status_current / status_history       |
          +---------------------------------------------------+
```

## Testing strategy

- **Checks:** unit-tested against mock inputs (trait makes this trivial), each
  status/message path covered — including `Unknown` on failure to run.
- **Rollup:** table-driven tests over component combinations (critical vs
  non-critical, mixed statuses) asserting the parent status and message.
- **Scheduler/debounce:** focused tests that N-consecutive results are required
  before a transition commits.
- **Notifiers:** behind the trait, exercised with test doubles so tests never
  actually page anyone.

## Plan 2 design decisions (added 2026-07-22, after Plan 1 merged)

Decisions made when scoping the runtime, refining the sections above:

- **Monitors are API-only.** Created/edited/deleted solely via the REST API
  (and later the web UI). The SQLite file is internal app state — never
  hand-edited by a user. No declarative monitor config file. This keeps the
  DB the single source of truth and avoids file-vs-DB reconciliation.
- **Config split by need-before-app:** only the bootstrap essentials come from
  **environment variables** — the bind interface/port and the DB path — with
  sensible defaults so bare `cargo run` works. Everything else, **including
  notifier configuration** (HA URL/token, ntfy server/topic), is stored in the
  DB and configured via REST/UI like any other app setting. No secrets in env.
- **Debounce default:** a state change commits after **2 consecutive** matching
  results. Per-monitor override is added alongside notifiers (Plan 2b), since
  that is when it starts to matter.
- **The runtime ships in two plans:**
  - **Plan 2a — Runnable daemon:** scheduler + debounce, the axum JSON API
    (`/api/v1/status`, monitors CRUD, `/check-types`, `/monitors/:id/run`),
    `main` wiring into a single binary, env config, and a `seed.sh` curl
    convenience for bootstrapping live tests. Runs the existing
    `http`/`tcp`/`frigate` checks. Deliverable: a curl-able daemon for live
    testing against real services.
  - **Plan 2b — Alerting & more checks:** notifier settings-in-DB + REST config
    + per-monitor selection (which notifiers, min severity); HA + ntfy
    notifiers; and the `cert-expiry`, `ping`, and `json-health` check types.
- **Deployment (later, not Plan 2):** a Dockerfile and a GitHub Action to build
  and push an image to ghcr. Running locally for now.

## Deferred / future

- Additional plugins: Music Assistant / Spotify, Unraid array & SMART, Plex,
  network/DNS/VPN.
- Frozen-frame detection for cameras.
- Auth, if it ever leaves the LAN.
- Richer history / metrics retention if needed.
