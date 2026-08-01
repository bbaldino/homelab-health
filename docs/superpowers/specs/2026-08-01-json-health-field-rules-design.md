# json-health Field Rules (interpret + threshold a JSON field) — Design

Date: 2026-08-01

## Summary

Extend the existing `json-health` check so that, in addition to surfacing a
service's self-reported `status`/`components`, it can evaluate user-authored
**field rules** against the same fetched JSON body. A field rule reads a named
field, **interprets** it (an RFC3339 timestamp → seconds remaining, or a raw
number), and applies **degraded/critical thresholds** — producing a graduated
Ok → Degraded → Critical health component that is appended to the monitor's
`components` array. Thresholds live centrally in the health monitor's config
(UI-tunable), not in the service.

Driving case: `caas.home/health` exposes `access_token_expires_at` (an RFC3339
timestamp) and `access_token_expires_in_seconds`, but `json-health` currently
ignores extra fields, so token expiry never affects health. A field rule keyed
on `access_token_expires_at` (interpret: timestamp; degraded < 1h; critical <
10m) makes the caas monitor degrade then go critical as the token nears expiry.

## Motivation & decisions (from brainstorming)

- **Thresholds live in health, not the service.** Central, UI-tunable, no
  service redeploy to change "how close is too close." (Rejected: caas
  self-reporting a component — clean but not centrally tunable.)
- **A generic field-interpretation hook, not an ad-hoc contract section.** health
  reads whatever field the service already emits and interprets it; no change to
  the `/health` contract and no change to caas. (Rejected: a standardized
  `expiries` section in the contract — felt too ad-hoc / over-specified.)
- **Keyed on the timestamp, computed by health.** For the timestamp
  interpretation health computes "seconds remaining" itself, so there is no
  stale-number or clock-skew dependency on the service's own countdown. A
  `number` interpretation is offered as a fallback for raw numeric fields.
- **Folded into `json-health`, not a separate check.** caas is already a
  `json-health` monitor; one monitor keeps surfacing the 4 contract components
  and adds the token component alongside them on the same card. (Rejected: a
  separate `json-field` check — would need a second monitor on the same URL.)
- **Reuses existing primitives.** The rules are a repeatable list in config,
  reusing the `FieldKind::List` schema kind + `Field.options` + the ListField
  form UI shipped with the prometheus check. No new UI primitive.

## The field-rule model

A rule transforms a JSON field into a number, then thresholds it into a
component:

```
name       component label, e.g. "access_token"        (required)
field      dotted path into the JSON body, e.g.         (required)
           "access_token_expires_at" (top-level is the common case;
           "a.b.c" traverses nested objects)
interpret  "timestamp" | "number"                       (required)
op         "<" | ">"   (default "<")                    (optional)
degraded   threshold that trips Degraded                (optional*)
critical   threshold that trips Critical                (optional*)
```

`*` at least one of `degraded`/`critical` is required (a rule with neither does
nothing).

**Interpretation** turns the field value into an `f64`:
- **`timestamp`** — the field is a string parsed as RFC3339; value = seconds
  until that instant = `expires_at - now` (negative once expired).
- **`number`** — the field is a JSON number; value = the number as-is.

**Thresholding** compares that value:
- `op = "<"` (default): `value < critical` → **Critical**; else `value <
  degraded` → **Degraded**; else **Ok**. (Natural for a countdown: fewer seconds
  remaining is worse. Expired ⇒ `value ≤ 0 < critical` ⇒ Critical.)
- `op = ">"`: `value > critical` → **Critical**; else `value > degraded` →
  **Degraded**; else **Ok**. (For raw numbers where bigger is worse.)
- Only the thresholds that are set are checked; omit `critical` for a
  warn-only rule that never reds the monitor, or omit `degraded` to jump
  straight to critical below the threshold.

Each rule produces **one component**, always `critical: true` (so its Critical
status propagates to the monitor rather than capping at Degraded — the graduated
`degraded`/`critical` thresholds are how you express "warn only": leave
`critical` unset). Component message is human-readable:
- timestamp: `expires in 42m` / `expired 5m ago`
- number: `12766 (< 3600)`

**Errors → Unknown component** (named for the field, so a mistake is visible
rather than silently passing):
- field absent at the path
- `timestamp` interpret but the value isn't a parseable RFC3339 string
- `number` interpret but the value isn't a JSON number

## Output: appended to `components`

Field-rule components are appended **after** the service's self-reported
components and rolled up together via the existing `rollup()`. The caas monitor
card becomes:

```
caas            degraded — access_token
  claude_binary   ok
  credentials     ok
  refresh_token   ok
  upstream_auth   ok
  access_token    degraded — expires in 42m     ← field rule
```

They are indistinguishable from service-reported components by design (you care
about caas's health, not who computed each piece).

Combining with the body's shape:
- Body has a `components` array (the caas case): final components =
  service components ++ field-rule components → `CheckReport::from_components`.
- Body is status-only (no `components`) but has field rules: the report status
  is the worst of the body's top-level status and the field-rule rollup; the
  field-rule components are listed.
- No components and no field rules: unchanged from today (status-only report).

## Config & schema

`json-health` config gains an optional `field_rules` list; everything else
(`url`, `timeout_secs`) is unchanged:

```json
{
  "url": "http://caas.home/health",
  "timeout_secs": 10,
  "field_rules": [
    { "name": "access_token", "field": "access_token_expires_at",
      "interpret": "timestamp", "degraded": 3600, "critical": 600 }
  ]
}
```

Schema (`FieldKind::List`, `required: false`) with item sub-fields:

| sub-field | kind | required | notes |
|---|---|---|---|
| `name` | string | yes | component label |
| `field` | string | yes | dotted JSON path |
| `interpret` | string (enum) | yes | `options`: `timestamp`, `number` |
| `op` | string (enum) | no | `options`: `<`, `>`; default `<` |
| `degraded` | float | no | at least one of degraded/critical required |
| `critical` | float | no | |

The config struct uses `#[serde(deny_unknown_fields)]`; bad config → the check
returns Unknown (as today). For `timestamp` interpret the `degraded`/`critical`
values are **seconds remaining** (i.e. "1h" is `3600`); the UI help text says so.

## Data flow

1. `run()` fetches the URL and parses the body once as `serde_json::Value`
   (today it deserializes straight to `HealthBody`; this changes to keep the raw
   `Value` so field rules can read arbitrary paths). Non-2xx bodies are still
   parsed (the contract allows a 503-with-body). Fetch/parse failures → Unknown,
   as today.
2. Deserialize `HealthBody` from the `Value` for the contract `status`/
   `components` (unchanged mapping; unparseable → Unknown as today).
3. `evaluate_field_rules(&Value, &[FieldRule], now) -> Vec<Component>` — pure and
   unit-tested with `now` injected (never calls `Utc::now()` internally).
4. Combine per "Output" above and roll up.

## Components / boundaries

- `src/check/json_health.rs` — add `FieldRule`, `Interpret`, `Op`, the config
  field `field_rules`; a `read_path(&Value, &str) -> Option<&Value>` helper; a
  pure `evaluate_field_rules(body: &Value, rules: &[FieldRule], now: DateTime<Utc>)`;
  extend `schema()` and `run()`. json-health's existing `evaluate(HealthBody)`
  stays for the contract mapping.
- `Cargo.toml` — `cargo add chrono` for RFC3339 parsing + `Utc::now()` (already
  present transitively via prometheus-parse).
- No API, scheduler, or store changes. UI: none beyond what the prometheus List
  field builder already provides — the `json-health` schema simply now includes
  a List field, which the existing `ListField`/`SchemaField`/`MonitorForm`
  render generically. (Autocomplete is prometheus-specific and not offered here.)

## Testing

- **Pure `evaluate_field_rules` unit tests** (with injected `now`): timestamp far
  → Ok; within `degraded` → Degraded; within `critical` → Critical; already
  expired → Critical; `number` with `<` and `>`; warn-only (no `critical`) caps
  at Degraded; missing field → Unknown; unparseable timestamp → Unknown;
  non-numeric `number` field → Unknown; dotted path into a nested object.
- **`read_path` unit tests**: top-level, nested, missing.
- **Integration (wiremock)**: serve a caas-shaped body (4 components +
  `access_token_expires_at`) with a field rule; assert the `access_token`
  component and the rolled-up monitor status; assert a healthy body with a
  far-off expiry stays Ok.
- **Schema test**: `json-health` schema exposes `field_rules` as a List with the
  right sub-fields and `interpret`/`op` options.
- **Frontend**: browser-verify that editing a `json-health` monitor shows the
  field-rules row builder and round-trips (reuses the prometheus List UI; no new
  components).

## Dependencies

- `cargo add chrono` (RFC3339 parse + `Utc::now`).

## Non-goals (YAGNI)

- No interpretations beyond `timestamp` and `number` (no unix-epoch, RFC2822,
  ISO-8601 durations, etc. — add later if needed).
- No arbitrary numeric metric monitoring from prometheus endpoints — that's the
  existing `prometheus` check; this is for fields already inside a `/health`
  JSON body.
- No autocomplete / endpoint introspection for `json-health` field rules.
- No change to the `/health` contract or to caas.
