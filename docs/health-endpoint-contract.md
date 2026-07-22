# Health Endpoint Contract (v1)

How a custom service should expose its health so the homelab health monitor can
consume it. This maps 1:1 onto the monitor's internal model, so a future
`json-health` check plugin can ingest it without translation.

> Note: the v1 `http` check inspects only the HTTP **status code**. Parsing the
> body described here requires the `json-health` check type (planned, not yet
> built). Implement this contract now so services are ready when it lands.

## Endpoint

- Method: `GET`
- Path: `/health` (configurable per monitor, but `/health` is the default)
- Response: `Content-Type: application/json`
- Should be cheap and side-effect free; safe to poll every 15–60s.
- No auth assumed (monitor runs on the LAN / behind your reverse proxy).

## Response body

```json
{
  "status": "ok",
  "message": "",
  "components": [
    { "name": "database", "status": "ok",       "critical": true,  "message": "" },
    { "name": "cache",    "status": "degraded",  "critical": false, "message": "hit rate 40%" }
  ]
}
```

### Fields

| Field                  | Type    | Required | Meaning |
|------------------------|---------|----------|---------|
| `status`               | string  | see note | The service's own overall verdict: `ok`, `degraded`, or `critical`. |
| `message`              | string  | when `status != ok` | Human-readable reason; surfaced verbatim in alerts and the UI. |
| `components`           | array   | optional | Sub-parts of the service, each independently reported. |
| `components[].name`    | string  | yes      | Stable identifier for the sub-part (e.g. `database`, `camera:driveway`). |
| `components[].status`  | string  | yes      | `ok`, `degraded`, or `critical`. |
| `components[].critical`| bool    | yes      | Whether this sub-part failing takes the whole service down. |
| `components[].message` | string  | when status `!= ok` | Reason for this sub-part's state. |

Note on `status`: a service **may omit the top-level `status`** and return only
`components` — the monitor will roll them up. If both are present, the monitor
trusts the components and rolls them up (top-level `status` is treated as a
summary, not an override).

### Status values

- `ok` — healthy.
- `degraded` — working but impaired (non-critical subsystem down, perf/quality loss).
- `critical` — not functioning; operator must act.
- Do **not** emit `unknown`. `unknown` is the monitor's own state for "couldn't
  reach or parse the service" — services never report it.

## Rollup rules (how the monitor combines components)

Match these so your top-level `status` agrees with the monitor's:

- A failing **critical** component propagates its severity to the service
  (`critical` critical-component ⇒ service `critical`).
- A failing **non-critical** component caps the service at **`degraded`** — it
  can never make the service `critical`.
- Service status = the worst effective status across components.

## HTTP status code

Pick one convention and keep it consistent — the future `json-health` check
will parse the body regardless of code, but the choice affects the fallback
`http` (code-only) check:

- **Recommended:** always return `200` and put the verdict in the body. The
  body is the source of truth.
- Alternative: return `503` when the service is `critical`, `200` otherwise.
  This lets even the code-only `http` check catch hard-down states, at the cost
  of the body not being read on `503`.

## Conventions

- Keep components **flat** — one level only. Model nesting via names
  (`camera:driveway`) rather than nested objects; the monitor's model is flat.
- Keep `name` values **stable** across restarts so history and alerts line up.
- Return quickly (target < 1s). If a real check is expensive, cache its result
  and report the cached value.

## Minimal examples

Healthy, no detail needed:
```json
{ "status": "ok" }
```

Degraded via a non-critical component (service stays up):
```json
{
  "components": [
    { "name": "spotify", "status": "critical", "critical": false, "message": "token refresh failing" }
  ]
}
```
→ monitor rolls this up to **degraded** (non-critical caps at degraded).

Critical:
```json
{
  "status": "critical",
  "message": "primary datastore unreachable",
  "components": [
    { "name": "database", "status": "critical", "critical": true, "message": "connection refused" }
  ]
}
```
