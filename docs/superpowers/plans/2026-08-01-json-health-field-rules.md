# json-health Field Rules Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the `json-health` check with optional **field rules** that read a named field from the fetched JSON body, interpret it (RFC3339 timestamp → seconds remaining, or a raw number), and threshold it into a graduated Ok→Degraded→Critical component appended to the monitor's `components` array — driving the caas token-expiry case with central, UI-tunable thresholds and no change to caas.

**Architecture:** All changes are in `src/check/json_health.rs`. A pure `evaluate_field_rules(&Value, &[FieldRule], now)` maps each rule to one component; `run()` now parses the body as a raw `serde_json::Value` (so rules can read arbitrary fields), deserializes `HealthBody` from it for the existing contract mapping, then merges contract components with field-rule components and rolls up. Timestamps are parsed with `chrono`. The rules are a repeatable list in config, reusing the existing `FieldKind::List` + `Field.options` schema kinds and the `ListField` form UI shipped with the prometheus check — so there is **no frontend code change**.

**Tech Stack:** Rust (serde, serde_json, `chrono`, reqwest, wiremock for tests). Preact/TS UI unchanged.

## Global Constraints

- **Conventional commits** required. Only `feat:`/`fix:` cut a release (release-plz `release_commits`), so use `feat:` for the user-facing capability.
- Format with `cargo +nightly fmt` before every commit.
- **CI gate now includes `cargo clippy --all-targets --all-features -- -D warnings`** (plus nightly fmt + `cargo test --locked`). Run all three locally before committing; the local `stable` toolchain must match CI's `@stable` (currently 1.97.1) or clippy drifts.
- Add Rust deps with `cargo add` (latest version).
- Acronyms: capitalize only the first letter of multi-letter acronyms.
- Config structs keep `#[serde(deny_unknown_fields)]`; bad config → the check returns `Status::Unknown` (never panics).
- **Component invariant:** `Component::new` debug-asserts a non-`Ok` status carries a non-empty message. Every field-rule component (including Unknown error components) must have a message.
- **Do not regress existing `json-health` tests.** All current tests in `src/check/json_health.rs` must still pass after the `run()` refactor (especially `parses_body_even_on_503`, `invalid_service_status_is_unknown`, `status_only_no_components`, `unreachable_is_unknown`, `bad_config_is_unknown`).
- Field-rule semantics (from the spec): interpret `timestamp` (→ seconds until, negative once expired) or `number` (raw); `op` default `<`; graduated `degraded`/`critical` thresholds (at least one required); each rule → one component with `critical: true`; missing field / unparseable value / no thresholds → an **Unknown** component naming the field/rule.
- Output: field-rule components are **appended after** the service's self-reported components and rolled up together via the existing `rollup()`.
- Work on branch `feat/json-health-field-rules`.

---

### Task 1: Dependency, rule types, config, and path reader

Adds the supporting pieces with unit tests; no `evaluate`/`schema`/`run` changes yet, so existing behavior is untouched.

**Files:**
- Modify: `Cargo.toml` (via `cargo add chrono`)
- Modify: `src/check/json_health.rs`

**Interfaces:**
- Produces (in `crate::check::json_health`):
  - `enum Interpret { Timestamp, Number }` (serde `rename_all = "snake_case"` → `"timestamp"`, `"number"`)
  - `enum Op { Lt, Gt }` (serde `rename` `"<"`, `">"`; `impl Default` → `Lt`)
  - `struct FieldRule { name: String, field: String, interpret: Interpret, op: Op (default), degraded: Option<f64>, critical: Option<f64> }` (Deserialize, `deny_unknown_fields`)
  - `JsonHealthConfig` gains `field_rules: Vec<FieldRule>` (`#[serde(default)]`)
  - `fn read_path<'a>(body: &'a Value, path: &str) -> Option<&'a Value>`

- [ ] **Step 1: Add chrono**

Run: `cargo add chrono`
(Already present transitively via prometheus-parse; this makes it a direct dep. Default features include `clock` for `Utc::now` and RFC3339 parsing.)

- [ ] **Step 2: Write failing unit tests** — append to the `tests` module in `src/check/json_health.rs`:

```rust
#[test]
fn read_path_top_level_and_nested_and_missing() {
    let v = json!({ "a": 1, "b": { "c": "x" } });
    assert_eq!(read_path(&v, "a"), Some(&json!(1)));
    assert_eq!(read_path(&v, "b.c"), Some(&json!("x")));
    assert_eq!(read_path(&v, "b.missing"), None);
    assert_eq!(read_path(&v, "nope"), None);
}

#[test]
fn config_accepts_field_rules_with_defaults() {
    let cfg: JsonHealthConfig = serde_json::from_value(json!({
        "url": "http://x/health",
        "field_rules": [
            { "name": "tok", "field": "access_token_expires_at",
              "interpret": "timestamp", "degraded": 3600, "critical": 600 }
        ]
    })).unwrap();
    assert_eq!(cfg.field_rules.len(), 1);
    let r = &cfg.field_rules[0];
    assert!(matches!(r.interpret, Interpret::Timestamp));
    assert!(matches!(r.op, Op::Lt));           // default
    assert_eq!(r.critical, Some(600.0));
}

#[test]
fn config_still_rejects_unknown_and_defaults_empty_rules() {
    let empty: JsonHealthConfig =
        serde_json::from_value(json!({ "url": "http://x" })).unwrap();
    assert!(empty.field_rules.is_empty());
    assert!(serde_json::from_value::<JsonHealthConfig>(json!({ "url": "http://x", "bogus": 1 })).is_err());
    // op parses the symbol form
    let r: FieldRule = serde_json::from_value(json!({
        "name": "n", "field": "f", "interpret": "number", "op": ">", "degraded": 80
    })).unwrap();
    assert!(matches!(r.op, Op::Gt));
}
```

- [ ] **Step 3: Run — expect failure**

Run: `cargo test -p homelab-health check::json_health::tests::read_path_top_level_and_nested_and_missing 2>&1 | tail -15`
Expected: FAIL — `read_path`, `Interpret`, `Op`, `FieldRule`, `field_rules` not defined.

- [ ] **Step 4: Implement the types, config field, and reader** in `src/check/json_health.rs`. Add the `chrono` import at the top (`use chrono::{DateTime, Utc};` — `DateTime`/`Utc` are used in Task 2, harmless here) and:

```rust
#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Interpret {
    Timestamp,
    Number,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = ">")]
    Gt,
}

impl Default for Op {
    fn default() -> Self {
        Op::Lt
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct FieldRule {
    pub name: String,
    pub field: String,
    pub interpret: Interpret,
    #[serde(default)]
    pub op: Op,
    #[serde(default)]
    pub degraded: Option<f64>,
    #[serde(default)]
    pub critical: Option<f64>,
}

/// Traverse a dotted path (`a.b.c`) into a JSON object. Any non-object segment
/// or missing key yields None.
fn read_path<'a>(body: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = body;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}
```

Add the config field to `JsonHealthConfig`:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonHealthConfig {
    url: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default)]
    field_rules: Vec<FieldRule>,
}
```

- [ ] **Step 5: Format, run these tests + the full existing json_health suite**

Run: `cargo +nightly fmt && cargo test -p homelab-health check::json_health 2>&1 | tail -20`
Expected: the three new tests PASS and every pre-existing `json_health` test still passes.

- [ ] **Step 6: Clippy + commit**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3` (must be clean)

```bash
git add Cargo.toml Cargo.lock src/check/json_health.rs
git commit -m "feat: json-health field-rule config types and path reader

Adds FieldRule/Interpret/Op, a field_rules list on the config
(deny_unknown_fields, defaults empty), a dotted-path JSON reader, and the
chrono dependency. No evaluation or schema wiring yet."
```

---

### Task 2: Pure `evaluate_field_rules`

The rule-to-component logic, fully unit-tested with an injected `now`.

**Files:** Modify `src/check/json_health.rs`

**Interfaces:**
- Consumes: `FieldRule`, `Interpret`, `Op`, `read_path` (Task 1); `Component`/`Status` (already imported).
- Produces: `fn evaluate_field_rules(body: &Value, rules: &[FieldRule], now: DateTime<Utc>) -> Vec<Component>`

- [ ] **Step 1: Write failing tests** in the `tests` module (inject `now` so they're deterministic):

```rust
use chrono::{TimeZone, Utc};

fn at(s: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn ts_rule(deg: f64, crit: f64) -> FieldRule {
    FieldRule { name: "tok".into(), field: "exp".into(),
        interpret: Interpret::Timestamp, op: Op::Lt, degraded: Some(deg), critical: Some(crit) }
}

#[test]
fn timestamp_far_out_is_ok() {
    let now = at("2026-08-01T00:00:00Z");
    let body = json!({ "exp": "2026-08-01T05:00:00Z" });   // 5h out
    let c = evaluate_field_rules(&body, &[ts_rule(3600.0, 600.0)], now);
    assert_eq!(c[0].status, Status::Ok);
    assert!(c[0].critical);
}

#[test]
fn timestamp_within_degraded_and_critical() {
    let now = at("2026-08-01T00:00:00Z");
    let deg = json!({ "exp": "2026-08-01T00:30:00Z" });   // 30m out → < 3600
    assert_eq!(evaluate_field_rules(&deg, &[ts_rule(3600.0, 600.0)], now)[0].status, Status::Degraded);
    let crit = json!({ "exp": "2026-08-01T00:05:00Z" });  // 5m out → < 600
    assert_eq!(evaluate_field_rules(&crit, &[ts_rule(3600.0, 600.0)], now)[0].status, Status::Critical);
}

#[test]
fn expired_timestamp_is_critical() {
    let now = at("2026-08-01T00:00:00Z");
    let body = json!({ "exp": "2026-07-31T23:59:00Z" });  // 1m ago
    assert_eq!(evaluate_field_rules(&body, &[ts_rule(3600.0, 600.0)], now)[0].status, Status::Critical);
}

#[test]
fn number_lt_and_gt() {
    let now = at("2026-08-01T00:00:00Z");
    let lt = FieldRule { name: "n".into(), field: "v".into(), interpret: Interpret::Number,
        op: Op::Lt, degraded: Some(100.0), critical: Some(10.0) };
    assert_eq!(evaluate_field_rules(&json!({"v": 5}), &[lt.clone()], now)[0].status, Status::Critical);
    assert_eq!(evaluate_field_rules(&json!({"v": 50}), &[lt.clone()], now)[0].status, Status::Degraded);
    assert_eq!(evaluate_field_rules(&json!({"v": 500}), &[lt], now)[0].status, Status::Ok);
    let gt = FieldRule { name: "n".into(), field: "v".into(), interpret: Interpret::Number,
        op: Op::Gt, degraded: Some(80.0), critical: Some(95.0) };
    assert_eq!(evaluate_field_rules(&json!({"v": 99}), &[gt], now)[0].status, Status::Critical);
}

#[test]
fn warn_only_rule_never_criticals() {
    let now = at("2026-08-01T00:00:00Z");
    let r = FieldRule { name: "tok".into(), field: "exp".into(), interpret: Interpret::Timestamp,
        op: Op::Lt, degraded: Some(3600.0), critical: None };
    let body = json!({ "exp": "2026-07-31T00:00:00Z" });  // long expired
    assert_eq!(evaluate_field_rules(&body, &[r], now)[0].status, Status::Degraded);
}

#[test]
fn missing_bad_and_no_threshold_are_unknown() {
    let now = at("2026-08-01T00:00:00Z");
    // missing field
    assert_eq!(evaluate_field_rules(&json!({}), &[ts_rule(3600.0, 600.0)], now)[0].status, Status::Unknown);
    // timestamp interpret but not a valid timestamp
    assert_eq!(evaluate_field_rules(&json!({"exp": "nope"}), &[ts_rule(3600.0, 600.0)], now)[0].status, Status::Unknown);
    // number interpret but not a number
    let numr = FieldRule { name: "n".into(), field: "v".into(), interpret: Interpret::Number,
        op: Op::Lt, degraded: Some(1.0), critical: None };
    assert_eq!(evaluate_field_rules(&json!({"v": "x"}), &[numr], now)[0].status, Status::Unknown);
    // no thresholds set
    let none = FieldRule { name: "n".into(), field: "v".into(), interpret: Interpret::Number,
        op: Op::Lt, degraded: None, critical: None };
    assert_eq!(evaluate_field_rules(&json!({"v": 1}), &[none], now)[0].status, Status::Unknown);
}
```

- [ ] **Step 2: Run — expect failure**

Run: `cargo test -p homelab-health check::json_health::tests::timestamp_within_degraded_and_critical 2>&1 | tail -15`
Expected: FAIL — `evaluate_field_rules` not found.

- [ ] **Step 3: Implement** in `src/check/json_health.rs`:

```rust
/// Human-friendly remaining/elapsed time for a timestamp rule's message.
fn humanize_remaining(secs: f64) -> String {
    let s = secs as i64;
    let a = s.abs();
    let (h, m) = (a / 3600, (a % 3600) / 60);
    let mag = if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{a}s")
    };
    if s >= 0 {
        format!("expires in {mag}")
    } else {
        format!("expired {mag} ago")
    }
}

/// Pure: turn each field rule into one component (Unknown on any read/parse/
/// config problem). `now` is injected so this is deterministic in tests.
pub fn evaluate_field_rules(
    body: &Value,
    rules: &[FieldRule],
    now: DateTime<Utc>,
) -> Vec<Component> {
    rules
        .iter()
        .map(|rule| {
            let unknown = |msg: String| Component::new(rule.name.clone(), Status::Unknown, true, msg);

            if rule.degraded.is_none() && rule.critical.is_none() {
                return unknown(format!("rule '{}' has no thresholds", rule.name));
            }
            let raw = match read_path(body, &rule.field) {
                Some(v) => v,
                None => return unknown(format!("field '{}' not found", rule.field)),
            };

            // interpret → (numeric value, message-friendly rendering)
            let (value, render): (f64, String) = match rule.interpret {
                Interpret::Timestamp => match raw.as_str().and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s).ok()
                }) {
                    Some(dt) => {
                        let secs = (dt.with_timezone(&Utc) - now).num_seconds() as f64;
                        (secs, humanize_remaining(secs))
                    }
                    None => return unknown(format!("field '{}' is not an RFC3339 timestamp", rule.field)),
                },
                Interpret::Number => match raw.as_f64() {
                    Some(n) => (n, format!("{n}")),
                    None => return unknown(format!("field '{}' is not a number", rule.field)),
                },
            };

            let breach = |threshold: Option<f64>| match (rule.op, threshold) {
                (Op::Lt, Some(t)) => value < t,
                (Op::Gt, Some(t)) => value > t,
                (_, None) => false,
            };
            let status = if breach(rule.critical) {
                Status::Critical
            } else if breach(rule.degraded) {
                Status::Degraded
            } else {
                Status::Ok
            };
            Component::new(rule.name.clone(), status, true, render)
        })
        .collect()
}
```

- [ ] **Step 4: Format, run the tests**

Run: `cargo +nightly fmt && cargo test -p homelab-health check::json_health::tests 2>&1 | tail -25`
Expected: all Step 1 tests PASS.

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3` (clean)

```bash
git add src/check/json_health.rs
git commit -m "feat: evaluate_field_rules for json-health

Pure mapping from field rules to components: interpret a field as an RFC3339
timestamp (seconds remaining) or a number, apply graduated degraded/critical
thresholds (op < or >), one component per rule (critical), Unknown on missing/
unparseable/no-threshold. now is injected for deterministic tests."
```

---

### Task 3: Wire into `schema()` and `run()`, merge, integration tests

**Files:** Modify `src/check/json_health.rs`

**Interfaces:**
- Consumes: `evaluate_field_rules` (Task 2), `field_rules` config (Task 1).
- Produces: `json-health` `schema()` advertising `field_rules`; `run()` that parses the raw body, merges contract + field-rule components, and rolls up.

- [ ] **Step 1: Write a failing merge/`run` test** in the `tests` module (a caas-shaped body):

```rust
const CAAS: &str = r#"{"status":"ok","message":"","components":[
  {"name":"credentials","status":"ok","critical":true,"message":""}
],"access_token_expires_at":"2026-08-01T00:20:00Z"}"#;

#[tokio::test]
async fn run_appends_field_rule_component() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(CAAS))
        .mount(&server)
        .await;
    // 20m-out token; degraded < 1h, critical < 10m → Degraded overall
    let report = JsonHealthCheck.run(&json!({
        "url": server.uri(),
        "field_rules": [{ "name": "access_token", "field": "access_token_expires_at",
            "interpret": "timestamp", "degraded": 3600, "critical": 600 }]
    })).await;
    assert_eq!(report.status, Status::Degraded);
    assert!(report.components.iter().any(|c| c.name == "access_token" && c.status == Status::Degraded));
    assert!(report.components.iter().any(|c| c.name == "credentials"));
}
```

Note: this test's `now` is real (`Utc::now()` inside `run`), so the fixture timestamp must be relative to now, not absolute. **Change the fixture to build the body at test time** with an `access_token_expires_at` 20 minutes in the future:

```rust
#[tokio::test]
async fn run_appends_field_rule_component() {
    let exp = (chrono::Utc::now() + chrono::Duration::minutes(20)).to_rfc3339();
    let body = json!({
        "status": "ok", "message": "",
        "components": [{ "name": "credentials", "status": "ok", "critical": true, "message": "" }],
        "access_token_expires_at": exp
    }).to_string();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server).await;
    let report = JsonHealthCheck.run(&json!({
        "url": server.uri(),
        "field_rules": [{ "name": "access_token", "field": "access_token_expires_at",
            "interpret": "timestamp", "degraded": 3600, "critical": 600 }]
    })).await;
    assert_eq!(report.status, Status::Degraded);
    assert!(report.components.iter().any(|c| c.name == "access_token" && c.status == Status::Degraded));
    assert!(report.components.iter().any(|c| c.name == "credentials"));
}
```

(Delete the `CAAS` const / first version — keep only this relative-time test.)

- [ ] **Step 2: Run — expect failure** (field rules not yet wired into `run`)

Run: `cargo test -p homelab-health check::json_health::tests::run_appends_field_rule_component 2>&1 | tail -15`
Expected: FAIL — the `access_token` component is absent (run ignores field_rules).

- [ ] **Step 3: Refactor `run()` to parse raw `Value`, and add a merge that folds field-rule components in.** Replace the body-parse + `evaluate` tail of `run()`:

```rust
        // Parse once as a raw Value so field rules can read arbitrary fields,
        // then map the contract shape from it. Parse regardless of HTTP status.
        let value: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => return CheckReport::new(Status::Unknown, format!("invalid health body: {e}")),
        };
        let body: HealthBody = match serde_json::from_value(value.clone()) {
            Ok(b) => b,
            Err(e) => return CheckReport::new(Status::Unknown, format!("invalid health body: {e}")),
        };

        let field_components = evaluate_field_rules(&value, &cfg.field_rules, chrono::Utc::now());
        JsonHealthCheck::evaluate(body, field_components)
```

Change `evaluate` to take the field components and merge (append after contract components; preserve the status-only path and the top-level status when there are no contract components):

```rust
    /// Pure mapping from a parsed body + field-rule components to a CheckReport.
    fn evaluate(body: HealthBody, field_components: Vec<Component>) -> CheckReport {
        let mut components: Vec<Component> = body
            .components
            .into_iter()
            .map(|c| {
                let status = Status::from(c.status);
                Component::new(c.name, status, c.critical, ensure_message(status, c.message))
            })
            .collect();
        let had_contract = !components.is_empty();
        components.extend(field_components);

        if had_contract {
            // component-bearing body: roll everything up (body.status is derived)
            return CheckReport::from_components(components);
        }
        if components.is_empty() {
            // no contract components and no field rules → status-only behavior
            return match body.status {
                Some(s) => {
                    let status = Status::from(s);
                    CheckReport::new(status, ensure_message(status, body.message))
                }
                None => CheckReport::new(Status::Unknown, "health body had neither status nor components"),
            };
        }
        // status-only body + field rules: worst of the two
        let field = CheckReport::from_components(components);
        let base = body.status.map(Status::from).unwrap_or(Status::Ok);
        if base.rank() >= field.status.rank() {
            CheckReport {
                status: base,
                message: ensure_message(base, body.message),
                components: field.components,
            }
        } else {
            field
        }
    }
```

(The existing `evaluate` callers are only `run` and the unit tests; update the unit tests that call `evaluate` to pass `vec![]` as the second arg — see Step 5.)

- [ ] **Step 4: Extend `schema()`** — add the `field_rules` List field after `timeout_secs` (inside the `fields: vec![...]`):

```rust
                Field {
                    name: "field_rules",
                    kind: FieldKind::List,
                    required: false,
                    default: None,
                    help: "Threshold rules over fields in the JSON body",
                    secret: false,
                    options: None,
                    fields: Some(vec![
                        Field { name: "name", kind: FieldKind::String, required: true, default: None,
                            help: "Component name", secret: false, options: None, fields: None },
                        Field { name: "field", kind: FieldKind::String, required: true, default: None,
                            help: "JSON field path, e.g. access_token_expires_at (dotted for nested)",
                            secret: false, options: None, fields: None },
                        Field { name: "interpret", kind: FieldKind::String, required: true, default: None,
                            help: "How to read the field", secret: false,
                            options: Some(vec![json!("timestamp"), json!("number")]), fields: None },
                        Field { name: "op", kind: FieldKind::String, required: false, default: Some(json!("<")),
                            help: "Comparison (worse when value crosses the threshold)", secret: false,
                            options: Some(vec![json!("<"), json!(">")]), fields: None },
                        Field { name: "degraded", kind: FieldKind::Float, required: false, default: None,
                            help: "Degraded threshold (for timestamp: seconds remaining, e.g. 3600 = 1h)",
                            secret: false, options: None, fields: None },
                        Field { name: "critical", kind: FieldKind::Float, required: false, default: None,
                            help: "Critical threshold (for timestamp: seconds remaining, e.g. 600 = 10m)",
                            secret: false, options: None, fields: None },
                    ]),
                },
```

- [ ] **Step 5: Update the two existing `evaluate` unit tests** that call `JsonHealthCheck::evaluate(parse(json!(...)))` — they now pass field components. Change each call from `JsonHealthCheck::evaluate(parse(json!({...})))` to `JsonHealthCheck::evaluate(parse(json!({...})), vec![])`. (`critical_critical_component_makes_report_critical`, `noncritical_critical_component_caps_at_degraded`, `status_only_no_components`, `empty_body_is_unknown`, `non_ok_component_missing_message_gets_fallback` — every `evaluate(` call in the tests.)

- [ ] **Step 6: Add an integration test for a healthy far-off token staying Ok** (guards against always-degrading):

```rust
#[tokio::test]
async fn run_far_off_token_stays_ok() {
    let exp = (chrono::Utc::now() + chrono::Duration::hours(3)).to_rfc3339();
    let body = json!({ "status": "ok", "components": [], "access_token_expires_at": exp }).to_string();
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_string(body)).mount(&server).await;
    let report = JsonHealthCheck.run(&json!({
        "url": server.uri(),
        "field_rules": [{ "name": "access_token", "field": "access_token_expires_at",
            "interpret": "timestamp", "degraded": 3600, "critical": 600 }]
    })).await;
    assert_eq!(report.status, Status::Ok);
}
```

- [ ] **Step 7: Add a schema test** confirming the List field + options:

```rust
#[test]
fn schema_exposes_field_rules_list() {
    let s = JsonHealthCheck.schema();
    let fr = s.fields.iter().find(|f| f.name == "field_rules").unwrap();
    assert!(matches!(fr.kind, FieldKind::List));
    let sub = fr.fields.as_ref().unwrap();
    let interp = sub.iter().find(|f| f.name == "interpret").unwrap();
    assert!(interp.options.as_ref().unwrap().contains(&json!("timestamp")));
}
```

- [ ] **Step 8: Format, full suite, clippy**

Run: `cargo +nightly fmt && cargo test -p homelab-health 2>&1 | tail -6`
Expected: all tests pass (every pre-existing json_health test + the new ones; total count up).
Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3` (clean)

- [ ] **Step 9: Commit**

```bash
git add src/check/json_health.rs
git commit -m "feat: json-health evaluates field rules and appends their components

run() now parses the body as a raw Value, maps the contract shape from it, and
appends evaluate_field_rules() components (rolled up together). schema() gains
a field_rules List (interpret/op dropdowns; timestamp thresholds in seconds
remaining). Existing status/component behavior is unchanged."
```

---

### Frontend (no code change — verification only)

The `field_rules` List is rendered by the existing generic `ListField`/`SchemaField`/`MonitorForm` (shipped with the prometheus check); `json-health` simply now advertises a List field. No UI code changes.

**Verification (done by the orchestrator, not a task):** build the UI, run a local daemon, edit/create a `json-health` monitor, confirm the "Field rules" row builder renders (name/field/interpret dropdown/op dropdown/degraded/critical), add a rule against a caas-shaped endpoint, save, and confirm it round-trips and evaluates (Run now). Live-verify against `caas.home/health` if reachable. Restore `ui/dist/.gitkeep` after any local UI build (vite deletes it).

---

## Self-Review

**Spec coverage:**
- Field-rule model (field/interpret/op/degraded/critical → component) → Tasks 1–2. ✅
- `timestamp` (seconds remaining) + `number` interpretations → Task 2 `evaluate_field_rules`. ✅
- Graduated degraded→critical, `op` default `<`, warn-only, at-least-one-threshold → Task 2 + tests. ✅
- Errors (missing/unparseable/no-threshold) → Unknown component → Task 2 + tests. ✅
- Appended to `components`, rolled up; status-only + component-bearing bodies → Task 3 `evaluate` merge + tests. ✅
- Config `field_rules` + schema List with interpret/op options → Tasks 1 & 3. ✅
- chrono via `cargo add` → Task 1. ✅
- Reuse of the List form UI, no frontend code → Frontend section. ✅
- Existing json_health behavior preserved → Global Constraints + Task 3 Step 5 (update evaluate call-sites) + Step 8. ✅

**Placeholder scan:** none — every step carries concrete code/tests.

**Type consistency:** `FieldRule`/`Interpret`/`Op`/`read_path`/`evaluate_field_rules` defined in Tasks 1–2 and used with matching signatures in Task 3. `evaluate` gains a second parameter (`Vec<Component>`) in Task 3 — Step 5 explicitly updates all existing call-sites, so no stale one-arg calls remain. `humanize_remaining` is internal to Task 2. Schema `field_rules` sub-field names (`name`/`field`/`interpret`/`op`/`degraded`/`critical`) match the `FieldRule` serde fields.
