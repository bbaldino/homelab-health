# Prometheus Metrics Check — Design

Date: 2026-07-31

## Summary

Add a `prometheus` check type. It monitors a service that exposes a Prometheus
metrics endpoint by fetching that endpoint, parsing the text exposition format,
and evaluating a user-authored list of **rules** against selected metric series
to produce health status. One monitor instance = one metrics URL + N rules,
each rule contributing one or more health components that roll up into the
monitor's status via the existing rollup logic.

This is the natural sibling of the `json-health` / `unraid` checks (fetch →
parse → evaluate → components), with one genuinely new piece: it is the first
check whose *config* holds a user-authored variable-length list (the rules), so
it introduces a reusable "list of objects" field kind in the config schema and
the form UI.

## Motivation

Homelab services increasingly expose Prometheus metrics (e.g. Backrest at
`http://backups.home/metrics`). We want to fold a chosen subset of those metrics
into the health model without standing up a full Prometheus + Alertmanager
stack. The rule model is a deliberately simplified Prometheus alerting rule:
select a series, compare its value to a threshold, assign severity.

## Non-goals (YAGNI)

- **No PromQL**, no aggregation functions (sum/avg/max/count across series).
- **No rates / deltas over time.** A once-per-interval check sees only the
  instantaneous value; computing rates would require remembering prior scrapes.
  Counters are supported only as instantaneous value comparisons (rarely useful)
  — we do not special-case them.
- **No histogram/summary quantile logic** beyond treating each emitted sample
  (including `_bucket`/`_sum`/`_count` series) as a plain named series.
- **No `for:` durations.** The scheduler already debounces (2 consecutive) before
  committing a state change, which covers flap prevention.
- **No regex label matchers** — equality matchers only.

## The rule model

A rule is a simplified Prometheus alert:

```
{ metric: string, labels: string, op: ">"|">="|"<"|"<="|"=="|"!=", threshold: f64, critical: bool }
```

- `metric` — the metric family name, e.g. `backrest_last_task_status`.
- `labels` — optional Prometheus matcher syntax, e.g. `task_type="backup",repo_id="unraid"`.
  Empty = match every series of that name. Equality only.
- `op` + `threshold` — the comparison; a **breach** is when `value <op> threshold`
  is true.
- `critical` — rollup weight (defaults false).

**Series selection:** a rule selects every parsed sample whose metric name equals
`metric` AND whose labels satisfy every matcher (`sample.labels[key] == value`).

**Fan-out:** each matched series becomes one component. Pinning all labels selects
one series → one component; leaving labels loose selects many → one component
each (exactly like Frigate's per-camera components). This is the same mechanism
for both "watch one series" and "watch the whole family".

**Status per component:**
- breach → **Critical** if the rule is `critical`, else **Degraded**
- no breach → **Ok**
- rule matched zero series → **Unknown** (see error handling)

Component name: `metric{labels}` of the matched sample, e.g.
`backrest_last_task_status{plan_id="stacks",repo_id="unraid",task_type="backup"}`.
Component message carries the observed value and the condition, e.g. `1 (!= 0)`.

Float values are compared exactly for `==`/`!=`. This is correct for the
integer-valued status/count gauges these rules target (value `0` = success,
warning counts, `up` 0/1) and matches Prometheus's own behavior. No epsilon.

Worked example (Backrest):

| metric | labels | op | threshold | critical | meaning |
|---|---|---|---|---|---|
| `backrest_last_task_status` | `task_type="backup"` | `!=` | 0 | ✔ | last backup task failed |
| `backrest_backup_file_warnings` | *(blank)* | `>` | 0 | | any backup emitted file warnings |
| `backrest_tasks_duration_secs` | `task_type="backup"` | `>` | 300 | | backup took over 5 minutes |

## Config schema

`prometheus` config value:

```json
{
  "url": "http://backups.home/metrics",
  "timeout_secs": 10,
  "rules": [
    { "metric": "backrest_last_task_status",     "labels": "task_type=\"backup\"", "op": "!=", "threshold": 0, "critical": true },
    { "metric": "backrest_backup_file_warnings", "labels": "",                     "op": ">",  "threshold": 0, "critical": false }
  ]
}
```

Top-level fields:

| field | kind | required | default | notes |
|---|---|---|---|---|
| `url` | string | yes | — | the metrics endpoint |
| `timeout_secs` | int | no | 10 | fetch timeout |
| `rules` | **list** | yes | — | array of rule objects (see below); empty → Unknown |

Rule sub-fields (the item shape of the `rules` list):

| sub-field | kind | required | default | notes |
|---|---|---|---|---|
| `metric` | string | yes | — | metric family name |
| `labels` | string | no | `""` | Prometheus equality matchers, or blank for all series |
| `op` | string (enum) | yes | — | one of `>` `>=` `<` `<=` `==` `!=` (rendered as dropdown) |
| `threshold` | float | yes | — | compared against the series value |
| `critical` | bool | no | false | rollup weight |

The config struct uses `#[serde(deny_unknown_fields)]`; `op` deserializes into a
typed enum. A malformed config (bad `op`, missing required sub-field) → the check
returns Unknown "invalid config", consistent with the other checks.

## Schema / UI extensions (new, reusable)

The current config schema (`ConfigSchema { fields: Vec<Field> }`, `Field` with a
scalar `FieldKind` of String/Int/Float/Bool) only expresses flat scalar fields.
Two additions, both reusable by any future check:

1. **`FieldKind::List`** plus an optional `fields: Vec<Field>` on `Field`
   describing each list item's sub-fields. A List field's stored value is a JSON
   array of objects shaped by those sub-fields.
2. **`options: Option<Vec<Value>>`** on `Field` — a fixed set of allowed values so
   the UI renders a `<select>` (used for `op`). Absent = free input as today.

UI (`ui/src/`):
- `SchemaField` renders a `List` field as a repeatable set of rows (one per array
  element), each row built from the item sub-fields, with add / remove controls;
  and renders a field with `options` as a dropdown.
- `MonitorForm` collects/preserves the array value for List fields (coercing each
  sub-field's scalar type as it already does for flat fields; omitting
  null-coerced optional sub-fields as it already does).
- The rule builder wires a **"Fetch metrics"** action (enabled once `url` is
  filled) that calls the inspect endpoint and drives autocomplete: the `metric`
  input suggests real metric names, and once a metric is chosen the `labels`
  input suggests that metric's label keys and observed values.

The list-of-objects field kind is generic; only the `prometheus` schema uses it
initially, and the rule-builder autocomplete is `prometheus`-specific wiring
layered on top.

## Inspect endpoint (autocomplete backend)

The browser cannot reach internal HTTP metrics endpoints, but the monitor server
can. A read-only endpoint powers autocomplete:

`POST /api/v1/checks/prometheus/inspect` with body `{ url, timeout_secs? }` →

```json
{ "metrics": {
    "backrest_last_task_status": { "labels": { "task_type": ["backup","forget","hook"], "repo_id": ["unraid","_unassociated_"] } },
    "backrest_backup_file_warnings": { "labels": { "plan_id": ["stacks"], "repo_id": ["unraid"] } }
} }
```

It fetches + parses the endpoint (sharing the check's fetch/parse helper) and
returns each metric name with its observed label keys → sorted, de-duplicated
values. It persists nothing. Fetch/parse failure → a non-2xx response with a
message the form surfaces ("couldn't fetch: connection refused").

## Data flow (per check run)

1. GET `url` with `timeout_secs` (reqwest, rustls — already a dependency).
2. Parse the body into samples (name, labels map, f64 value) using
   `prometheus-parse`. (Fallback: a small hand parser over the line-oriented
   exposition format if the crate's API proves awkward for label maps — the
   format is simple.)
3. `evaluate(scrape, &rules) -> CheckReport` (pure, unit-tested): for each rule,
   parse its matcher, select samples, emit one component per matched series (or
   one Unknown component if none matched), status per the rule model above.
4. `rollup()` (existing) combines components into the monitor status.

## Error handling

| condition | result | rationale |
|---|---|---|
| endpoint unreachable / non-2xx | whole monitor **Critical** (`cannot scrape …: <err>` / `HTTP 503`) | exporter/service down *is* the signal, same as the `http` check |
| 200 but body not parseable as metrics | **Unknown** ("response was not Prometheus metrics") | reached it but cannot judge health (e.g. an HTML auth page) |
| a rule matches zero series | **Unknown** component ("no series matched `metric{labels}`") | a missing critical metric surfaces Unknown overall — the exporter stopped emitting it |
| a rule's label matcher is malformed | that rule → one **Unknown** component naming the bad matcher | other rules still evaluate |
| no rules configured | **Unknown** ("no rules configured") | green-while-checking-nothing would be a lie |
| invalid config (bad `op`, etc.) | **Unknown** ("invalid config") | consistent with other checks' `deny_unknown_fields` handling |

The scheduler's timeout wrapper (→ Unknown) and debounce (2 consecutive) apply as
for every check.

## Components / boundaries

- `src/check/prometheus.rs` — config structs (`PrometheusConfig`, `Rule`, `Op`
  enum), a label-matcher parser, a shared `fetch_and_parse(url, timeout)` helper,
  a pure `evaluate(scrape, &[Rule]) -> CheckReport`, and `run()` wiring fetch →
  parse → evaluate. Registered in `Registry::with_builtins` (count 6 → 7).
- `src/check/mod.rs` — `FieldKind::List`, `Field.fields`, `Field.options`; the
  `with_builtins_registers_all` test asserts 7 and includes `prometheus`.
- `src/api.rs` — the `inspect` handler + route, reusing `fetch_and_parse`.
- `ui/src/` — `SchemaField` (List rows + options dropdown), `MonitorForm` (array
  values), and the rule-builder "Fetch metrics" autocomplete.

## Testing

- **Pure `evaluate` unit tests:** each op breach / no-breach; critical → Critical
  vs non-critical → Degraded; zero-match → Unknown; label matcher selects the
  right series; loose labels fan out to N components; malformed matcher → Unknown
  component; empty rules → Unknown. Plus label-matcher parser tests.
- **Integration (wiremock):** serve the real Backrest sample as a fixture → run →
  assert per-component statuses; non-200 → Critical; unparseable body → Unknown.
  Same harness for the inspect endpoint (metric/label map; unreachable → error).
- **Schema test:** builtins now 7; the `prometheus` schema exposes the `rules`
  List field with the right sub-fields and `op` options.
- **Frontend:** Playwright smoke — add a `prometheus` monitor, "Fetch metrics",
  add a couple of rules via the builder, save, confirm it lands and runs
  (live-verified against the real endpoint, as with the other checks).

## Dependencies

- `cargo add prometheus-parse` (added via cargo per repo convention, not pinned by
  hand). reqwest is already present.

## Decisions (resolved during design)

- Interpretation: **A** — consume a service's metrics as a health check (not
  exposing our own `/metrics`; that would be a separate feature).
- Rule model = simplified Prometheus alert; **instantaneous value comparisons
  only**; equality label matchers.
- **One component per matched series** (fan-out) is the single mechanism for
  "watch one" vs "watch the family".
- **`critical` rule breach → Critical, non-critical breach → Degraded**, matching
  the existing rollup.
- Labels entered as a **single matcher string**, not a nested key/value form.
- Error mapping per the table above.
- Autocomplete via a **read-only server-side inspect endpoint**.
