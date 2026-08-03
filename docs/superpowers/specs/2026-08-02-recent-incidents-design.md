# Recent Incidents (surface failures that already recovered) — Design

Date: 2026-08-02

## Summary

Expose **incidents** — bounded periods during which a monitor was not Ok — as a
first-class part of the JSON API, so that a service which failed and recovered
leaves a visible trace instead of silently returning to green.

Today `/api/v1/status` reports only *current* status. A monitor that went
Critical at 03:12 and recovered by 03:41 is indistinguishable from one that has
been healthy all week. The data to reconstruct that outage already exists
(`status_transitions`, kept indefinitely; `check_samples`, kept 7 days), but
nothing surfaces it, and a dashboard polling every 60s cannot observe a failure
that started and cleared between two polls — nor can it remember anything across
its own restarts.

The server already knows. It should say so.

## Motivation & decisions (from brainstorming)

- **Incidents, not raw failed runs.** A 30-minute outage at a 60s interval is
  *one* incident, not 30 log lines. Grouping happens once, server-side, from the
  debounced transitions — rather than every consumer reimplementing it and each
  getting the edge cases (gaps, restarts, mid-outage escalation) slightly wrong.
  Payload then scales with number of outages, not with check frequency ×
  duration. (Rejected: exposing raw non-Ok samples. Its one real advantage —
  catching sub-debounce blips — is noted as deferred below.)
- **Both inline and on demand.** A bounded summary inline in `/api/v1/status` so
  a dashboard needs no second request for the at-a-glance case, plus a dedicated
  endpoint for depth and for history older than the inline window. (Rejected:
  endpoint-only — a naive consumer just won't make the second call; and
  inline-only — no way to ask "what happened last month.")
- **Ongoing incidents included,** with `ended_at: null`. This is the only way to
  answer "critical since when?" — `status_current.updated_at` is the last *check*
  time, not the time trouble started.
- **Any non-Ok status opens an incident** — degraded, critical, and unknown.
  Unknown is a real event: a check that couldn't reach the service at 3am is
  exactly the outage worth seeing. Consumers filter on `worst_status`.
- **Per-component detail is a union over the incident's lifetime,** not a
  snapshot. Components fail at different moments — disk3 at 03:12, free space at
  03:30 — so any single sample tells half the story.
- **No migration.** `status_transitions` already carries `status`, `message`, and
  `at`; `check_samples` already carries `components_json`. This is derivation over
  data already being recorded.

## The incident model

An incident is a maximal contiguous period of non-Ok committed status.

```
started_at        epoch secs; transition into non-Ok. May precede the query window.
ended_at          epoch secs of the transition back to Ok; null while ongoing.
duration_secs     ended_at - started_at, or now - started_at when ongoing.
worst_status      worst status reached during the incident.
message           rollup message from the transition where worst_status was
                  first reached.
failing_components components that were non-Ok at any point (see below).
```

Derivation rules:

- **Opens** on a committed transition into any non-Ok status; **closes** on the
  next committed transition to Ok.
- **Escalation merges.** Degraded 03:12 → Critical 03:20 → Ok 03:41 is one
  incident: 29 minutes, `worst_status: critical`. Ties in severity keep the
  earlier message — the line that explains how it got that bad.
- **Ongoing** incidents carry `ended_at: null`. There is deliberately no separate
  `resolved` boolean: two fields that can disagree is a bug waiting to happen.
- **`duration_secs` is included** even though it is derivable, because for an
  ongoing incident the consumer would otherwise need to know the server's clock.
- **A brand-new monitor must not fake an incident.** If no transition exists at
  or before the window start, prior status is Unknown-by-absence and *no*
  incident opens. Absence of data is not evidence of failure. This is a
  deliberate divergence from `compute_uptime`, which honestly counts that span as
  `unknown_secs`.

### failing_components

Only components that were non-Ok at some point during the incident; healthy
components during an outage are noise.

```
name           component name
worst_status   worst status this component reached during the incident
critical       the component's critical flag
message        the component's message at its worst moment (earlier wins on ties)
first_seen     epoch secs, first sample in the incident where it was non-Ok
last_seen      epoch secs, last sample in the incident where it was non-Ok
```

`first_seen`/`last_seen` distinguish "both failed together" from "disk3 was down
the whole 29 minutes and free space only blipped at the end" — effectively free
once the samples are being scanned anyway.

`message` and `failing_components` are **not** redundant. `http` and `tcp` checks
emit no components at all, so `failing_components` is empty for them and the
rollup `message` is the only description of the failure that exists.

Because component detail comes from `check_samples`, it is subject to the 7-day
prune. Incidents older than the retention window degrade gracefully:
`failing_components` becomes `[]` while timing, `worst_status`, and `message`
survive indefinitely from `status_transitions`.

## API surface

### Inline: `GET /api/v1/status`

Each monitor gains `recent_incidents` — incidents **overlapping the last 24h**,
the 5 newest, **without** `failing_components`:

```json
{
  "id": 7,
  "name": "unraid",
  "status": "ok",
  "message": "array started, 8 disks healthy",
  "components": [ ... ],
  "recent_incidents": [
    {
      "started_at": 1754032320,
      "ended_at": 1754034060,
      "duration_secs": 1740,
      "worst_status": "critical",
      "message": "2 of 8 components unhealthy"
    }
  ]
}
```

Enough to render "green, but 2 incidents overnight" plus a tooltip. Component
detail is deliberately excluded to keep the polling path cheap: it costs **two
extra queries per poll total** — one batched transitions query and one batched
prior-transition query across all monitors — rather than per-monitor work.

Bounds (24h, 5) are fixed constants, not query parameters; consumers wanting more
use the endpoint below.

### On demand: `GET /api/v1/incidents`

Cross-monitor, newest first, with full detail.

Query parameters:

```
since       epoch secs, default now - 7d
until       epoch secs, default now
monitor_id  optional filter
limit       default 50, clamped 1..500   (matches existing endpoint conventions)
```

Each entry adds `monitor_id` and `monitor_name` to the incident shape, plus
`failing_components`:

```json
{
  "monitor_id": 7,
  "monitor_name": "unraid",
  "started_at": 1754032320,
  "ended_at": 1754034060,
  "duration_secs": 1740,
  "worst_status": "critical",
  "message": "2 of 8 components unhealthy",
  "failing_components": [
    { "name": "disk3", "worst_status": "critical", "critical": true,
      "message": "SMART: FAILING_NOW",
      "first_seen": 1754032320, "last_seen": 1754034000 },
    { "name": "cache free space", "worst_status": "degraded", "critical": false,
      "message": "4% free (threshold 10%)",
      "first_seen": 1754033400, "last_seen": 1754034000 }
  ]
}
```

Incidents overlapping the requested range are returned whole — `started_at` may
precede `since`. Truncating an incident at an arbitrary query boundary would
misreport its duration.

The sample scan backing `failing_components` is uncapped. A 3-day outage at 60s
intervals is ~4300 rows of JSON to parse and fold: a few milliseconds in SQLite,
and only on this on-demand path. A truncation flag was rejected as something no
consumer could act on.

## Implementation

### `src/incident.rs` (new)

Mirrors `src/uptime.rs`: a pure function, unit-testable without a database.

```rust
pub fn compute_incidents(
    prior: Option<(Status, i64)>,   // transition that OPENED the incident in
                                    // force at the window start (see below)
    transitions: &[TransitionRow],  // ascending by `at`, since window start
    now: i64,
) -> Vec<Incident>                  // newest first, failing_components empty
```

`prior` is **not** simply the last transition at or before the window start. A
monitor can commit several non-Ok transitions in a row with no intervening Ok —
the scheduler's debounce is in-memory, so a process restart re-commits the status
the monitor is already in — and the last row of such a run is not when the outage
began. Seeding from it reports `started_at` too late, and *differently for every
window size*, which is precisely what "incidents overlapping the requested range
are returned whole" forbids. `prior` is therefore the **earliest non-Ok
transition after the most recent Ok transition at or before the window start**,
and `None` when the monitor was Ok there or has no transitions at all — both
meaning no incident was open going into the window.

`failing_components` is populated separately by the store, only for the
`/incidents` path — keeping the grouping logic pure and DB-free.

### `src/store.rs`

- `TransitionRow { status, message, at }`. `get_transitions_since` currently
  returns `(Status, i64)` and drops the message; widen it to return
  `TransitionRow` and have the uptime caller ignore the extra field. One
  representation of a transition row, not two.
- `last_transition_at_or_before(monitor_id, ts) -> Option<TransitionRow>` —
  generalizes the existing `status_at`, which returns the status but not when it
  started. This is the **uptime** query: it reports the status actually in force
  at `ts`, `Ok` included, and must keep doing so.
- `open_incident_start(monitor_id, ts) -> Option<TransitionRow>` — the
  **incident** query: the earliest non-Ok transition after the most recent Ok
  transition at or before `ts`, i.e. the row that opened the run rather than its
  tail. `None` when the monitor was Ok at `ts` or has no transitions.
- Batched variants of all three, keyed by monitor, for the `/status` path. The
  existing `list_status` N+1 is out of scope, but this must not add to it.
- `failing_components_for(monitor_id, start, end) -> Vec<FailingComponent>` —
  scans samples in range, folds per component name.

### `src/api.rs`

- `MonitorStatus` response gains `recent_incidents`.
- New route `GET /api/v1/incidents`.

### UI (`ui/src/`)

- `types.ts`: `Incident`, `FailingComponent`; `MonitorStatus.recent_incidents`.
- `MonitorCard`: a badge on cards whose `recent_incidents` is non-empty —
  `2 incidents · 24h` — so the all-green board stops lying.
- `MonitorDetail`: an incidents list above the existing flat sample history,
  fetched from `/incidents?monitor_id=`, each row expandable to its
  `failing_components`.
- A cross-monitor recent-incidents view on the board, driven by
  `GET /api/v1/incidents` — its own layout with loading/empty/error states,
  answering "what happened overnight?" in one read.

## Phasing

**Phase 1 — API.** `incident.rs`, store queries, both endpoints, tests. Ships and
releases on its own (`feat:` → release-plz cuts a version → GHCR image), so
external dashboard consumers are unblocked before any UI work starts.

**Phase 2 — UI: card badge + detail list.** The reference consumer; validates the
contract end-to-end.

**Phase 3 — UI: cross-monitor feed.** The second surface.

## Testing

`compute_incidents` is pure, so the cases that matter need no database:

- escalation (degraded → critical → ok) merges into **one** incident with
  `worst_status: critical` and the message from the escalation
- flap (down, up, down, up) produces **two** incidents
- ongoing incident yields `ended_at: null` and `duration_secs` measured to `now`
- prior status non-Ok at window start opens an incident at its *real*
  `started_at`, before the window
- a re-committed status inside the window (restart) folds into the open incident
  instead of restating when it began

Store-level, for `open_incident_start`: a run of consecutive non-Ok transitions
with no intervening Ok returns the **earliest** row of the run; a monitor that
was Ok at `ts` returns `None`; and the same incident queried through a 1-day and
a 7-day window reports an identical `started_at` and `duration_secs`.
- **no transitions at all → empty vec** (the brand-new-monitor case)
- unknown opens an incident like any other non-Ok status

Store-level: the `failing_components` fold across multiple samples (union, worst
status per component, first/last seen), and an incident whose samples have been
pruned returning `[]` without error.

API-level: `recent_incidents` present and bounded in `/status`; `/incidents`
filtering by `monitor_id` and `since`; `limit` clamping; unknown `monitor_id`
returning an empty list rather than 404 (it is a filter, not a path segment).

## Out of scope / deferred

- **Sub-debounce blips.** A single failed run that self-heals before the debounce
  threshold never becomes a transition, so it produces no incident. Could later
  be surfaced as a lightweight `blips_24h` counter from raw samples, without
  inflating the incident list.
- **Snapshotting components into `status_transitions`.** Would make component
  detail survive the 7-day prune, but transitions only fire on rollup status
  changes — a component failing mid-incident without changing the rollup would be
  missed. The sample scan is strictly more accurate.
- **Alerting on incidents.** Notifier work remains Plan 2b.
- **Fixing the `list_status` N+1.** Pre-existing; this design only commits to not
  making it worse.
