# Prometheus Metrics Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `prometheus` check type that scrapes a service's metrics endpoint, parses the exposition format, and evaluates user-authored rules (metric + label matchers + comparison) into per-series health components — plus a reusable list-of-objects config-schema/form field kind and a read-only inspect endpoint that powers metric/label autocomplete.

**Architecture:** A new `src/check/prometheus.rs` follows the existing fetch→parse→`evaluate`→`rollup` pattern (like `json_health.rs`/`unraid.rs`). The exposition format is parsed with the `prometheus-parse` crate into a small internal `Series` type; a pure `evaluate(&[Series], &[Rule])` maps rules to components. The config schema gains a generic `List` field kind (a repeatable group of typed sub-fields) and an `options` list (enum dropdowns), consumed by an extended Preact form. A `POST /api/v1/checks/prometheus/inspect` endpoint fetches+parses an endpoint and returns its metric/label map for autocomplete.

**Tech Stack:** Rust (tokio, axum 0.8, reqwest/rustls, serde, `prometheus-parse`), wiremock for tests; Preact + TypeScript for the UI.

## Global Constraints

- **Conventional commits** required (`feat:`/`fix:`/`ci:`/`docs:`…); the repo's release-plz automation depends on them.
- Rust is formatted with `cargo +nightly fmt` (run before every commit).
- Add Rust deps with `cargo add` (never hand-edit versions) — this gets the latest.
- Acronyms: capitalize only the first letter of multi-letter acronyms (`RagService`, not `RAGService`).
- Config structs use `#[serde(deny_unknown_fields)]`; bad config → the check returns `Status::Unknown` (never panics), matching the other checks.
- **Component invariant:** `Component::new` debug-asserts that a non-`Ok` status carries a non-empty message. Every non-Ok component must have a message.
- New `Field` schema attributes must be `#[serde(skip_serializing_if = "Option::is_none")]` so existing `/check-types` responses are byte-for-byte unchanged.
- Rule semantics (from the spec): **instantaneous value comparisons only** (no rates/PromQL); **equality label matchers only**; **one component per matched series** (fan-out); **critical-rule breach → Critical, non-critical breach → Degraded, no breach → Ok, zero matches → Unknown**.
- Error mapping (from the spec): endpoint unreachable or non-2xx → **Critical**; request timeout → **Unknown**; 200-but-unparseable body → **Unknown**; a rule matching zero series → **Unknown** component; malformed label matcher → **Unknown** component; no rules configured → **Unknown**.
- UI is TypeScript, not JavaScript.
- Work happens on branch `feat/prometheus-check`.

---

### Task 1: Extend the config schema with `List` and `options`

Adds the generic schema primitives the rule-builder needs, and updates every existing check's `Field { … }` literal so the codebase still compiles. No prometheus-specific code yet.

**Files:**
- Modify: `src/check/mod.rs` (the `FieldKind` enum and `Field` struct)
- Modify (mechanical): `src/check/http.rs`, `src/check/tcp.rs`, `src/check/frigate.rs`, `src/check/json_health.rs`, `src/check/music_assistant.rs`, `src/check/unraid.rs` — every `Field { … }` literal gains `options: None, fields: None,`.

**Interfaces:**
- Produces:
  - `FieldKind::List` variant (serializes as `"list"`).
  - `Field.options: Option<Vec<serde_json::Value>>` — fixed allowed values (enum dropdown). `#[serde(skip_serializing_if = "Option::is_none")]`.
  - `Field.fields: Option<Vec<Field>>` — sub-field schema for a `List` item. `#[serde(skip_serializing_if = "Option::is_none")]`.

- [ ] **Step 1: Write a failing test** in `src/check/mod.rs`'s `tests` module:

```rust
#[test]
fn list_field_with_options_serializes() {
    let f = Field {
        name: "rules",
        kind: FieldKind::List,
        required: false,
        default: None,
        help: "rules",
        secret: false,
        options: None,
        fields: Some(vec![Field {
            name: "op",
            kind: FieldKind::String,
            required: true,
            default: None,
            help: "comparison",
            secret: false,
            options: Some(vec![serde_json::json!(">"), serde_json::json!("!=")]),
            fields: None,
        }]),
    };
    let v = serde_json::to_value(&f).unwrap();
    assert_eq!(v["kind"], "list");
    assert_eq!(v["fields"][0]["name"], "op");
    assert_eq!(v["fields"][0]["options"][0], ">");
    // Absent optionals must be omitted, not null (keeps existing responses stable).
    let scalar = serde_json::to_value(&f.fields.as_ref().unwrap()[0]).unwrap();
    let plain = serde_json::to_value(Field {
        name: "url", kind: FieldKind::String, required: true, default: None,
        help: "u", secret: false, options: None, fields: None,
    }).unwrap();
    assert!(plain.get("options").is_none());
    assert!(plain.get("fields").is_none());
    let _ = scalar;
}
```

- [ ] **Step 2: Run it — expect a compile error** (the new fields don't exist yet).

Run: `cargo test -p homelab-health check::tests::list_field_with_options_serializes 2>&1 | tail -20`
Expected: FAIL — `Field` has no field named `options`/`fields`, and `FieldKind::List` not found.

- [ ] **Step 3: Add the variant and fields** in `src/check/mod.rs`:

```rust
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    String,
    Int,
    Float,
    Bool,
    List,
}

#[derive(Clone, Debug, Serialize)]
pub struct Field {
    pub name: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<Value>,
    pub help: &'static str,
    pub secret: bool,
    /// Fixed set of allowed values → the UI renders a dropdown. None = free input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<Value>>,
    /// Sub-field schema for a `List` item. None for scalar fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<Field>>,
}
```

- [ ] **Step 4: Update every existing `Field { … }` literal** in the six check files to add `options: None,` and `fields: None,`. Compile to find them all:

Run: `cargo build -p homelab-health 2>&1 | grep -E "missing field|error\[" | head -40`
Fix each reported literal until the build is clean. (There are literals in `http.rs`, `tcp.rs`, `frigate.rs`, `json_health.rs`, `music_assistant.rs`, `unraid.rs`.)

- [ ] **Step 5: Format, then run the new test + the full suite**

Run: `cargo +nightly fmt && cargo test -p homelab-health 2>&1 | tail -20`
Expected: the new test PASSES and all existing tests still pass (including `music_assistant_token_field_is_secret` and `with_builtins_registers_all`, which are unaffected).

- [ ] **Step 6: Commit**

```bash
git add src/check/mod.rs src/check/http.rs src/check/tcp.rs src/check/frigate.rs src/check/json_health.rs src/check/music_assistant.rs src/check/unraid.rs
git commit -m "feat: add List field kind and options to config schema

Reusable list-of-objects field kind (a repeatable group of typed sub-fields)
plus an options list for enum dropdowns. New attributes skip-serialize when
None so existing check-type responses are unchanged."
```

---

### Task 2: Prometheus check core — deps, types, matcher parser, fetch+parse

Builds the supporting pieces in a new module with unit tests. No `CheckType` impl or registration yet (so `with_builtins` and its count test are untouched).

**Files:**
- Modify: `Cargo.toml` (via `cargo add prometheus-parse`)
- Create: `src/check/prometheus.rs`
- Modify: `src/check/mod.rs` — add `pub mod prometheus;` (next to the other `pub mod` lines)

**Interfaces:**
- Produces (all in `crate::check::prometheus`):
  - `struct Series { metric: String, labels: BTreeMap<String, String>, value: f64 }`
  - `enum Op { Gt, Ge, Lt, Le, Eq, Ne }` (serde-renamed to `> >= < <= == !=`) with `fn test(self, value: f64, threshold: f64) -> bool` and `fn as_str(self) -> &'static str`
  - `struct Rule { metric: String, labels: String, op: Op, threshold: f64, critical: bool }` (Deserialize, `deny_unknown_fields`, `labels`/`critical` default)
  - `struct PrometheusConfig { url: String, timeout_secs: u64, rules: Vec<Rule> }` (Deserialize, `deny_unknown_fields`, `timeout_secs` default 10, `rules` default empty)
  - `fn parse_matchers(s: &str) -> Result<Vec<(String, String)>, String>` — parses `k="v",k2="v2"`; empty → `vec![]`
  - `enum ScrapeError { Timeout, Unreachable(String), BadStatus(u16), Unparseable(String) }`
  - `async fn fetch_and_parse(url: &str, timeout_secs: u64) -> Result<Vec<Series>, ScrapeError>`

- [ ] **Step 1: Add the dependency**

Run: `cargo add prometheus-parse`
Then confirm the API surface actually installed (variant/field names can differ by version):
Run: `cargo doc -p prometheus-parse --no-deps 2>/dev/null; ls target/doc/prometheus_parse/ 2>/dev/null | head`
The code below assumes `prometheus_parse::Scrape::parse(impl Iterator<Item = io::Result<String>>) -> Result<Scrape>`, `Scrape.samples: Vec<Sample>`, `Sample { metric: String, labels: Labels, value: Value }`, `Labels` iterable as `(&String, &String)` with `.get(&str)`, and `Value::{Gauge,Counter,Untyped}(f64)`. If the installed version differs, adjust the two helpers in Step 5 accordingly — the rest of the plan is insulated from the crate by the `Series` type.

- [ ] **Step 2: Declare the module.** In `src/check/mod.rs`, add alongside the existing `pub mod` lines:

```rust
pub mod prometheus;
```

- [ ] **Step 3: Write failing unit tests** in `src/check/prometheus.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_tests_compare_correctly() {
        assert!(Op::Ne.test(1.0, 0.0));
        assert!(!Op::Ne.test(0.0, 0.0));
        assert!(Op::Gt.test(2.0, 1.0));
        assert!(Op::Ge.test(1.0, 1.0));
        assert!(Op::Le.test(1.0, 1.0));
        assert!(Op::Eq.test(0.0, 0.0));
    }

    #[test]
    fn parse_matchers_handles_pairs_quotes_and_empty() {
        assert_eq!(parse_matchers("").unwrap(), Vec::<(String, String)>::new());
        assert_eq!(
            parse_matchers("task_type=\"backup\"").unwrap(),
            vec![("task_type".into(), "backup".into())]
        );
        assert_eq!(
            parse_matchers("a=\"1\", b=\"2\"").unwrap(),
            vec![("a".into(), "1".into()), ("b".into(), "2".into())]
        );
        assert!(parse_matchers("garbage no equals").is_err());
    }

    #[test]
    fn config_rejects_unknown_fields_and_bad_op() {
        let ok: Result<PrometheusConfig, _> = serde_json::from_value(serde_json::json!({
            "url": "http://x/metrics",
            "rules": [{ "metric": "m", "op": "!=", "threshold": 0 }]
        }));
        let cfg = ok.unwrap();
        assert_eq!(cfg.timeout_secs, 10);
        assert_eq!(cfg.rules[0].labels, "");
        assert!(!cfg.rules[0].critical);

        let bad_op: Result<PrometheusConfig, _> = serde_json::from_value(serde_json::json!({
            "url": "http://x", "rules": [{ "metric": "m", "op": "≈", "threshold": 0 }]
        }));
        assert!(bad_op.is_err());

        let unknown: Result<PrometheusConfig, _> = serde_json::from_value(serde_json::json!({
            "url": "http://x", "rules": [], "bogus": 1
        }));
        assert!(unknown.is_err());
    }
}
```

- [ ] **Step 4: Run — expect compile failure**

Run: `cargo test -p homelab-health check::prometheus 2>&1 | tail -20`
Expected: FAIL — `Op`, `parse_matchers`, `PrometheusConfig` not defined.

- [ ] **Step 5: Implement the core types and helpers** at the top of `src/check/prometheus.rs`:

```rust
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

/// A single scalar metric series, decoupled from the parser crate so `evaluate`
/// and its tests never touch `prometheus_parse` types.
#[derive(Clone, Debug)]
pub struct Series {
    pub metric: String,
    pub labels: BTreeMap<String, String>,
    pub value: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum Op {
    #[serde(rename = ">")] Gt,
    #[serde(rename = ">=")] Ge,
    #[serde(rename = "<")] Lt,
    #[serde(rename = "<=")] Le,
    #[serde(rename = "==")] Eq,
    #[serde(rename = "!=")] Ne,
}

impl Op {
    pub fn test(self, value: f64, threshold: f64) -> bool {
        match self {
            Op::Gt => value > threshold,
            Op::Ge => value >= threshold,
            Op::Lt => value < threshold,
            Op::Le => value <= threshold,
            Op::Eq => value == threshold,
            Op::Ne => value != threshold,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Gt => ">", Op::Ge => ">=", Op::Lt => "<",
            Op::Le => "<=", Op::Eq => "==", Op::Ne => "!=",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub metric: String,
    #[serde(default)]
    pub labels: String,
    pub op: Op,
    pub threshold: f64,
    #[serde(default)]
    pub critical: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrometheusConfig {
    pub url: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

fn default_timeout() -> u64 { 10 }

/// Parse Prometheus equality matchers: `k="v",k2="v2"`. Empty string → no
/// matchers. Not full PromQL — no commas inside values, equality only.
pub fn parse_matchers(s: &str) -> Result<Vec<(String, String)>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("bad matcher '{part}' (expected key=\"value\")"))?;
        let key = k.trim().to_string();
        if key.is_empty() {
            return Err(format!("bad matcher '{part}' (empty key)"));
        }
        let val = v.trim().trim_matches('"').to_string();
        out.push((key, val));
    }
    Ok(out)
}

pub enum ScrapeError {
    Timeout,
    Unreachable(String),
    BadStatus(u16),
    Unparseable(String),
}

fn sample_value(v: &prometheus_parse::Value) -> Option<f64> {
    use prometheus_parse::Value::*;
    match v {
        Counter(x) | Gauge(x) | Untyped(x) => Some(*x),
        _ => None, // histograms/summaries have no single scalar; skip.
    }
}

/// Fetch the endpoint and parse it into scalar series. Non-2xx → BadStatus,
/// reqwest timeout → Timeout, other transport errors → Unreachable, and a body
/// that isn't valid exposition format → Unparseable.
pub async fn fetch_and_parse(url: &str, timeout_secs: u64) -> Result<Vec<Series>, ScrapeError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| ScrapeError::Unreachable(e.to_string()))?;

    let resp = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            ScrapeError::Timeout
        } else {
            ScrapeError::Unreachable(e.to_string())
        }
    })?;

    let status = resp.status();
    if !status.is_success() {
        return Err(ScrapeError::BadStatus(status.as_u16()));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| ScrapeError::Unreachable(e.to_string()))?;

    let lines = body.lines().map(|l| Ok::<_, std::io::Error>(l.to_owned()));
    let scrape = prometheus_parse::Scrape::parse(lines)
        .map_err(|e| ScrapeError::Unparseable(e.to_string()))?;

    let series = scrape
        .samples
        .iter()
        .filter_map(|s| {
            let value = sample_value(&s.value)?;
            let labels = s
                .labels
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Some(Series {
                metric: s.metric.clone(),
                labels,
                value,
            })
        })
        .collect();
    Ok(series)
}
```

- [ ] **Step 6: Add a wiremock test** for `fetch_and_parse` in the `tests` module:

```rust
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SAMPLE: &str = "# TYPE up gauge\nup{job=\"a\"} 1\nup{job=\"b\"} 0\n";

#[tokio::test]
async fn fetch_and_parse_returns_scalar_series() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
        .mount(&server)
        .await;
    let series = fetch_and_parse(&server.uri(), 5).await.ok().unwrap();
    assert_eq!(series.len(), 2);
    let a = series.iter().find(|s| s.labels.get("job").map(String::as_str) == Some("a")).unwrap();
    assert_eq!(a.metric, "up");
    assert_eq!(a.value, 1.0);
}

#[tokio::test]
async fn fetch_and_parse_flags_non_2xx() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    assert!(matches!(fetch_and_parse(&server.uri(), 5).await, Err(ScrapeError::BadStatus(503))));
}
```

- [ ] **Step 7: Format and run the module's tests**

Run: `cargo +nightly fmt && cargo test -p homelab-health check::prometheus 2>&1 | tail -20`
Expected: all `check::prometheus::tests::*` PASS.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/check/prometheus.rs src/check/mod.rs
git commit -m "feat: prometheus check core — config, rules, matcher parser, scrape

Adds the prometheus-parse dependency and a Series type that decouples
evaluation from the parser. Config/Rule/Op with deny_unknown_fields, an
equality label-matcher parser, and fetch_and_parse mapping transport/status/
parse failures to a typed ScrapeError."
```

---

### Task 3: Evaluation, schema, run, registration

Assembles the `CheckType` from the Task 2 pieces: the pure `evaluate`, the config `schema()`, `run()` with error mapping, and registration in `with_builtins`.

**Files:**
- Modify: `src/check/prometheus.rs` (add `evaluate`, `struct PrometheusCheck`, the `CheckType` impl, more tests)
- Modify: `src/check/mod.rs` (register in `with_builtins`; update `with_builtins_registers_all`)

**Interfaces:**
- Consumes (Task 2): `Series`, `Op`, `Rule`, `PrometheusConfig`, `parse_matchers`, `fetch_and_parse`, `ScrapeError`.
- Produces: `pub struct PrometheusCheck;` implementing `CheckType` with `type_id() == "prometheus"`; `fn evaluate(series: &[Series], rules: &[Rule]) -> CheckReport`.

- [ ] **Step 1: Write failing `evaluate` tests** in `src/check/prometheus.rs`'s `tests` module:

```rust
fn series(metric: &str, labels: &[(&str, &str)], value: f64) -> Series {
    Series {
        metric: metric.into(),
        labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        value,
    }
}
fn rule(metric: &str, labels: &str, op: Op, threshold: f64, critical: bool) -> Rule {
    Rule { metric: metric.into(), labels: labels.into(), op, threshold, critical }
}

#[test]
fn empty_rules_is_unknown() {
    let r = evaluate(&[], &[]);
    assert_eq!(r.status, Status::Unknown);
    assert!(r.message.contains("no rules"));
}

#[test]
fn critical_breach_reds_the_monitor() {
    let s = vec![series("backrest_last_task_status", &[("task_type", "backup")], 1.0)];
    let r = evaluate(&s, &[rule("backrest_last_task_status", "task_type=\"backup\"", Op::Ne, 0.0, true)]);
    assert_eq!(r.status, Status::Critical);
    assert_eq!(r.components.len(), 1);
    assert_eq!(r.components[0].status, Status::Critical);
}

#[test]
fn noncritical_breach_caps_at_degraded() {
    let s = vec![series("warnings", &[], 3.0)];
    let r = evaluate(&s, &[rule("warnings", "", Op::Gt, 0.0, false)]);
    assert_eq!(r.status, Status::Degraded);
}

#[test]
fn no_breach_is_ok() {
    let s = vec![series("warnings", &[], 0.0)];
    let r = evaluate(&s, &[rule("warnings", "", Op::Gt, 0.0, false)]);
    assert_eq!(r.status, Status::Ok);
}

#[test]
fn loose_labels_fan_out_to_one_component_per_series() {
    let s = vec![
        series("status", &[("task", "backup")], 0.0),
        series("status", &[("task", "forget")], 0.0),
        series("status", &[("task", "hook")], 1.0),
    ];
    let r = evaluate(&s, &[rule("status", "", Op::Ne, 0.0, false)]);
    assert_eq!(r.components.len(), 3);
    assert_eq!(r.status, Status::Degraded); // the "hook" series breaches
}

#[test]
fn label_matcher_selects_one_series() {
    let s = vec![
        series("status", &[("task", "backup")], 0.0),
        series("status", &[("task", "hook")], 1.0),
    ];
    let r = evaluate(&s, &[rule("status", "task=\"backup\"", Op::Ne, 0.0, true)]);
    assert_eq!(r.components.len(), 1);
    assert_eq!(r.status, Status::Ok);
}

#[test]
fn zero_matches_is_unknown_component() {
    let r = evaluate(&[], &[rule("missing_metric", "", Op::Gt, 0.0, true)]);
    assert_eq!(r.components.len(), 1);
    assert_eq!(r.components[0].status, Status::Unknown);
    assert_eq!(r.status, Status::Unknown); // critical + unknown surfaces unknown
}

#[test]
fn malformed_matcher_is_unknown_component() {
    let s = vec![series("m", &[], 1.0)];
    let r = evaluate(&s, &[rule("m", "garbage", Op::Gt, 0.0, false)]);
    assert_eq!(r.components[0].status, Status::Unknown);
}
```

- [ ] **Step 2: Run — expect failure** (`evaluate` undefined).

Run: `cargo test -p homelab-health check::prometheus::tests::critical_breach_reds_the_monitor 2>&1 | tail -20`
Expected: FAIL — `evaluate` not found.

- [ ] **Step 3: Implement `evaluate` and the series-name helper** in `src/check/prometheus.rs`:

```rust
use crate::report::{CheckReport, Component};
use crate::status::Status;

fn series_name(metric: &str, labels: &BTreeMap<String, String>) -> String {
    if labels.is_empty() {
        metric.to_string()
    } else {
        let inner: Vec<String> = labels.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
        format!("{metric}{{{}}}", inner.join(","))
    }
}

/// Pure: map rules against parsed series into a rolled-up CheckReport.
pub fn evaluate(series: &[Series], rules: &[Rule]) -> CheckReport {
    if rules.is_empty() {
        return CheckReport::new(Status::Unknown, "no rules configured");
    }

    let mut components = Vec::new();
    for rule in rules {
        let matchers = match parse_matchers(&rule.labels) {
            Ok(m) => m,
            Err(e) => {
                components.push(Component::new(
                    format!("{} {{{}}}", rule.metric, rule.labels),
                    Status::Unknown,
                    rule.critical,
                    format!("invalid label matcher: {e}"),
                ));
                continue;
            }
        };

        let matched: Vec<&Series> = series
            .iter()
            .filter(|s| s.metric == rule.metric)
            .filter(|s| {
                matchers
                    .iter()
                    .all(|(k, v)| s.labels.get(k).map(String::as_str) == Some(v.as_str()))
            })
            .collect();

        if matched.is_empty() {
            let sel = if rule.labels.is_empty() {
                rule.metric.clone()
            } else {
                format!("{}{{{}}}", rule.metric, rule.labels)
            };
            components.push(Component::new(
                sel,
                Status::Unknown,
                rule.critical,
                "no series matched",
            ));
            continue;
        }

        for s in matched {
            let breached = rule.op.test(s.value, rule.threshold);
            let status = if !breached {
                Status::Ok
            } else if rule.critical {
                Status::Critical
            } else {
                Status::Degraded
            };
            let msg = format!("{} ({} {})", s.value, rule.op.as_str(), rule.threshold);
            components.push(Component::new(
                series_name(&s.metric, &s.labels),
                status,
                rule.critical,
                msg,
            ));
        }
    }

    CheckReport::from_components(components)
}
```

- [ ] **Step 4: Run the `evaluate` tests**

Run: `cargo test -p homelab-health check::prometheus::tests 2>&1 | tail -25`
Expected: all the Step 1 tests PASS.

- [ ] **Step 5: Implement `CheckType`** (schema, run) in `src/check/prometheus.rs`:

```rust
use super::{CheckType, ConfigSchema, Field, FieldKind};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PrometheusCheck;

impl PrometheusCheck {
    fn map_scrape_error(e: ScrapeError, url: &str) -> CheckReport {
        match e {
            ScrapeError::Timeout => {
                CheckReport::new(Status::Unknown, format!("timed out scraping {url}"))
            }
            ScrapeError::Unreachable(m) => {
                CheckReport::new(Status::Critical, format!("cannot scrape {url}: {m}"))
            }
            ScrapeError::BadStatus(code) => {
                CheckReport::new(Status::Critical, format!("{url} returned HTTP {code}"))
            }
            ScrapeError::Unparseable(m) => {
                CheckReport::new(Status::Unknown, format!("response was not Prometheus metrics: {m}"))
            }
        }
    }
}

#[async_trait]
impl CheckType for PrometheusCheck {
    fn type_id(&self) -> &'static str {
        "prometheus"
    }

    fn schema(&self) -> ConfigSchema {
        ConfigSchema {
            fields: vec![
                Field {
                    name: "url", kind: FieldKind::String, required: true, default: None,
                    help: "URL of the service's Prometheus metrics endpoint",
                    secret: false, options: None, fields: None,
                },
                Field {
                    name: "timeout_secs", kind: FieldKind::Int, required: false,
                    default: Some(json!(10)), help: "Request timeout in seconds",
                    secret: false, options: None, fields: None,
                },
                Field {
                    name: "rules", kind: FieldKind::List, required: false, default: None,
                    help: "Health rules evaluated against the scraped metrics",
                    secret: false, options: None,
                    fields: Some(vec![
                        Field {
                            name: "metric", kind: FieldKind::String, required: true, default: None,
                            help: "Metric family name", secret: false, options: None, fields: None,
                        },
                        Field {
                            name: "labels", kind: FieldKind::String, required: false,
                            default: Some(json!("")),
                            help: "Label matchers, e.g. task_type=\"backup\" (blank = all series)",
                            secret: false, options: None, fields: None,
                        },
                        Field {
                            name: "op", kind: FieldKind::String, required: true, default: None,
                            help: "Comparison", secret: false,
                            options: Some(vec![
                                json!(">"), json!(">="), json!("<"),
                                json!("<="), json!("=="), json!("!="),
                            ]),
                            fields: None,
                        },
                        Field {
                            name: "threshold", kind: FieldKind::Float, required: true, default: None,
                            help: "Value compared against the series", secret: false,
                            options: None, fields: None,
                        },
                        Field {
                            name: "critical", kind: FieldKind::Bool, required: false,
                            default: Some(json!(false)),
                            help: "Breach reds the monitor (vs. degrades it)",
                            secret: false, options: None, fields: None,
                        },
                    ]),
                },
            ],
        }
    }

    async fn run(&self, cfg: &Value) -> CheckReport {
        let cfg: PrometheusConfig = match serde_json::from_value(cfg.clone()) {
            Ok(c) => c,
            Err(e) => return CheckReport::new(Status::Unknown, format!("bad config: {e}")),
        };
        match fetch_and_parse(&cfg.url, cfg.timeout_secs).await {
            Ok(series) => evaluate(&series, &cfg.rules),
            Err(e) => PrometheusCheck::map_scrape_error(e, &cfg.url),
        }
    }
}
```

- [ ] **Step 6: Register the check and update the count test.** In `src/check/mod.rs`, add to `with_builtins`:

```rust
reg.register(Arc::new(crate::check::prometheus::PrometheusCheck));
```

Update `with_builtins_registers_all` to assert the new count and presence:

```rust
assert!(reg.get("prometheus").is_some());
assert_eq!(reg.schemas().len(), 7);
```

- [ ] **Step 7: Add `run()` integration tests** in `src/check/prometheus.rs` (a Backrest-shaped fixture + failure modes):

```rust
const BACKREST: &str = concat!(
    "# TYPE backrest_last_task_status gauge\n",
    "backrest_last_task_status{task_type=\"backup\"} 0\n",
    "backrest_last_task_status{task_type=\"hook\"} 1\n",
    "# TYPE backrest_backup_file_warnings gauge\n",
    "backrest_backup_file_warnings{plan_id=\"stacks\"} 0\n",
);

#[tokio::test]
async fn run_evaluates_rules_over_fetched_metrics() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(BACKREST))
        .mount(&server)
        .await;
    let cfg = json!({
        "url": server.uri(),
        "rules": [
            { "metric": "backrest_last_task_status", "labels": "task_type=\"backup\"", "op": "!=", "threshold": 0, "critical": true },
            { "metric": "backrest_backup_file_warnings", "op": ">", "threshold": 0 }
        ]
    });
    let report = PrometheusCheck.run(&cfg).await;
    assert_eq!(report.status, Status::Ok); // backup succeeded, no warnings
    assert_eq!(report.components.len(), 2);
}

#[tokio::test]
async fn run_non_2xx_is_critical() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let report = PrometheusCheck.run(&json!({ "url": server.uri(), "rules": [] })).await;
    assert_eq!(report.status, Status::Critical);
}

#[tokio::test]
async fn run_unparseable_body_is_unknown() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>login</html>"))
        .mount(&server)
        .await;
    let report = PrometheusCheck
        .run(&json!({ "url": server.uri(), "rules": [{ "metric": "m", "op": ">", "threshold": 0 }] }))
        .await;
    assert_eq!(report.status, Status::Unknown);
}

#[tokio::test]
async fn run_bad_config_is_unknown() {
    let report = PrometheusCheck.run(&json!({ "url": "http://x", "bogus": 1 })).await;
    assert_eq!(report.status, Status::Unknown);
}
```

Note: `<html>login</html>` must actually fail `Scrape::parse`. If the installed `prometheus-parse` tolerates it, change the body to one that provably fails (e.g. `"# TYPE\ngarbage line without value"`), confirming via a quick unit assertion.

- [ ] **Step 8: Format and run the whole suite**

Run: `cargo +nightly fmt && cargo test -p homelab-health 2>&1 | tail -20`
Expected: all tests PASS, including the updated `with_builtins_registers_all` (7) and every `check::prometheus` test.

- [ ] **Step 9: Commit**

```bash
git add src/check/prometheus.rs src/check/mod.rs
git commit -m "feat: prometheus check evaluation, schema, and registration

Pure evaluate() maps rules to one component per matched series (critical
breach -> Critical, non-critical -> Degraded, zero match -> Unknown), rolled
up as usual. schema() exposes url/timeout/rules (a List field with op options),
run() maps scrape failures per the spec, and the check is registered (7 builtins)."
```

---

### Task 4: Inspect endpoint for autocomplete

A read-only API endpoint that fetches+parses an endpoint and returns its metric/label map, reusing Task 2's `fetch_and_parse`.

**Files:**
- Modify: `src/check/prometheus.rs` (add `inspect_result` + serializable response structs)
- Modify: `src/api.rs` (route + handler)

**Interfaces:**
- Consumes: `fetch_and_parse`, `ScrapeError`, `Series`.
- Produces:
  - `pub fn inspect_result(series: &[Series]) -> InspectResult` where `InspectResult { metrics: BTreeMap<String, MetricInfo> }`, `MetricInfo { labels: BTreeMap<String, Vec<String>> }` (both `Serialize`).
  - Route `POST /api/v1/checks/prometheus/inspect`, body `{ url: String, timeout_secs: Option<u64> }`, response `200 InspectResult` or a non-2xx with a plain-text message.

- [ ] **Step 1: Write a failing test** for `inspect_result` in `src/check/prometheus.rs`:

```rust
#[test]
fn inspect_result_groups_label_values() {
    let s = vec![
        series("status", &[("task", "backup"), ("repo", "unraid")], 0.0),
        series("status", &[("task", "hook"), ("repo", "unraid")], 1.0),
        series("warnings", &[("plan", "stacks")], 0.0),
    ];
    let r = inspect_result(&s);
    let status = r.metrics.get("status").unwrap();
    assert_eq!(status.labels.get("task").unwrap(), &vec!["backup".to_string(), "hook".to_string()]);
    assert_eq!(status.labels.get("repo").unwrap(), &vec!["unraid".to_string()]); // de-duped
    assert!(r.metrics.contains_key("warnings"));
}
```

- [ ] **Step 2: Run — expect failure**

Run: `cargo test -p homelab-health check::prometheus::tests::inspect_result_groups_label_values 2>&1 | tail -15`
Expected: FAIL — `inspect_result`/`InspectResult` not defined.

- [ ] **Step 3: Implement `inspect_result` and the structs** in `src/check/prometheus.rs`:

```rust
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Serialize)]
pub struct MetricInfo {
    pub labels: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize)]
pub struct InspectResult {
    pub metrics: BTreeMap<String, MetricInfo>,
}

/// Build a metric-name → {label key → sorted unique values} map for autocomplete.
pub fn inspect_result(series: &[Series]) -> InspectResult {
    let mut acc: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    for s in series {
        let entry = acc.entry(s.metric.clone()).or_default();
        for (k, v) in &s.labels {
            entry.entry(k.clone()).or_default().insert(v.clone());
        }
    }
    let metrics = acc
        .into_iter()
        .map(|(metric, labels)| {
            let labels = labels
                .into_iter()
                .map(|(k, vs)| (k, vs.into_iter().collect::<Vec<_>>()))
                .collect();
            (metric, MetricInfo { labels })
        })
        .collect();
    InspectResult { metrics }
}
```

- [ ] **Step 4: Run the unit test**

Run: `cargo test -p homelab-health check::prometheus::tests::inspect_result_groups_label_values 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 5: Add the route + handler** in `src/api.rs`. Add the route inside `build_app` (near `check-types`):

```rust
.route("/api/v1/checks/prometheus/inspect", post(prometheus_inspect))
```

Add the handler and request type (near the other handlers). Reuse the existing `internal`/error style; a fetch/parse failure returns a readable message:

```rust
#[derive(serde::Deserialize)]
struct InspectRequest {
    url: String,
    timeout_secs: Option<u64>,
}

async fn prometheus_inspect(
    Json(req): Json<InspectRequest>,
) -> Result<Json<crate::check::prometheus::InspectResult>, (StatusCode, String)> {
    let timeout = req.timeout_secs.unwrap_or(10);
    match crate::check::prometheus::fetch_and_parse(&req.url, timeout).await {
        Ok(series) => Ok(Json(crate::check::prometheus::inspect_result(&series))),
        Err(e) => {
            let msg = match e {
                crate::check::prometheus::ScrapeError::Timeout => "timed out".to_string(),
                crate::check::prometheus::ScrapeError::Unreachable(m) => format!("unreachable: {m}"),
                crate::check::prometheus::ScrapeError::BadStatus(c) => format!("HTTP {c}"),
                crate::check::prometheus::ScrapeError::Unparseable(m) => format!("not Prometheus metrics: {m}"),
            };
            Err((StatusCode::BAD_GATEWAY, msg))
        }
    }
}
```

(`post` is already imported in `api.rs`; add `use crate::check::prometheus;` imports inline as above or at the top.)

- [ ] **Step 6: Add an endpoint integration test** in `src/api.rs`'s `tests` module (it already has a `spawn()` helper and `wiremock` imports):

```rust
#[tokio::test]
async fn prometheus_inspect_returns_metric_map() {
    let metrics_server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("# TYPE up gauge\nup{job=\"a\"} 1\n"))
        .mount(&metrics_server)
        .await;
    let (base, _store) = spawn().await;
    let body: Value = reqwest::Client::new()
        .post(format!("{base}/api/v1/checks/prometheus/inspect"))
        .json(&json!({ "url": metrics_server.uri() }))
        .send().await.unwrap()
        .json().await.unwrap();
    assert!(body["metrics"]["up"]["labels"]["job"].as_array().unwrap().contains(&json!("a")));
}

#[tokio::test]
async fn prometheus_inspect_unreachable_is_502() {
    let (base, _store) = spawn().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/checks/prometheus/inspect"))
        .json(&json!({ "url": "http://127.0.0.1:1/metrics", "timeout_secs": 1 }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 502);
}
```

- [ ] **Step 7: Format and run**

Run: `cargo +nightly fmt && cargo test -p homelab-health 2>&1 | tail -20`
Expected: all PASS.

- [ ] **Step 8: Commit**

```bash
git add src/check/prometheus.rs src/api.rs
git commit -m "feat: prometheus inspect endpoint for metric/label autocomplete

POST /api/v1/checks/prometheus/inspect fetches and parses an endpoint and
returns a metric-name -> label-key -> observed-values map. Read-only; shares
fetch_and_parse. Fetch/parse failures return 502 with a readable message."
```

---

### Task 5: UI — generic List field, options dropdown, form array handling

Renders `List` fields as repeatable rows and `options` as dropdowns, and teaches `MonitorForm` to initialize/coerce/submit array-valued config. Delivers a working rule builder (no autocomplete yet). Verified in the browser, matching how the other checks were validated.

**Files:**
- Modify: `ui/src/types.ts` (`Field` type)
- Create: `ui/src/components/ListField.tsx`
- Modify: `ui/src/components/SchemaField.tsx` (options dropdown; delegate `list`)
- Modify: `ui/src/components/MonitorForm.tsx` (array-valued config)
- Modify: `ui/src/styles.css` (row layout — minimal)

**Interfaces:**
- Consumes: the `/check-types` schema now including `kind: "list"`, `fields`, and `options` (Tasks 1 & 3).
- Produces: config submission where a List field's value is an array of objects (e.g. `config.rules = [{metric, labels, op, threshold, critical}, …]`).

- [ ] **Step 1: Extend the `Field` type** in `ui/src/types.ts`:

```ts
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
```

- [ ] **Step 2: Options dropdown + list delegation in `SchemaField.tsx`.** Before the scalar `<input>` branch, render a `<select>` when `field.options` is present:

```tsx
if (field.options && field.kind !== "bool" && field.kind !== "list") {
  return (
    <div class="form-field">
      <label class="field-label" for={inputId}>
        {label}
        {field.required && <span class="required-marker">*</span>}
      </label>
      <select
        id={inputId}
        value={toInputValue(value)}
        autoFocus={autoFocus}
        onChange={(e) => onChange(e.currentTarget.value)}
      >
        <option value="" disabled>—</option>
        {field.options.map((opt) => (
          <option key={String(opt)} value={String(opt)}>{String(opt)}</option>
        ))}
      </select>
      {field.help && <p class="field-help">{field.help}</p>}
    </div>
  );
}
```

And, at the top of `SchemaField`, delegate the list kind (import `ListField`):

```tsx
if (field.kind === "list") {
  return (
    <ListField
      field={field}
      value={Array.isArray(value) ? (value as Record<string, unknown>[]) : []}
      onChange={onChange}
    />
  );
}
```

- [ ] **Step 3: Create `ui/src/components/ListField.tsx`** — a generic repeatable-rows editor built from `field.fields`, reusing `SchemaField` for each sub-input:

```tsx
import type { Field } from "../types";
import { SchemaField, humanize } from "./SchemaField";

interface ListFieldProps {
  field: Field;
  value: Record<string, unknown>[];
  onChange: (value: unknown) => void;
}

function emptyRow(subFields: Field[]): Record<string, unknown> {
  const row: Record<string, unknown> = {};
  for (const f of subFields) {
    row[f.name] = f.kind === "bool" ? Boolean(f.default) : (f.default ?? "");
  }
  return row;
}

export function ListField({ field, value, onChange }: ListFieldProps) {
  const subFields = field.fields ?? [];
  const rows = value;

  function update(rows: Record<string, unknown>[]) {
    onChange(rows);
  }

  return (
    <div class="form-field list-field">
      <label class="field-label">{humanize(field.name)}</label>
      {field.help && <p class="field-help">{field.help}</p>}
      {rows.map((row, i) => (
        <div class="list-row" key={i}>
          {subFields.map((sf) => (
            <SchemaField
              key={sf.name}
              field={sf}
              value={row[sf.name]}
              onChange={(v) => {
                const next = rows.slice();
                next[i] = { ...row, [sf.name]: v };
                update(next);
              }}
            />
          ))}
          <button
            type="button"
            class="btn btn-secondary list-row-remove"
            onClick={() => update(rows.filter((_, j) => j !== i))}
            aria-label="Remove"
          >
            ✕
          </button>
        </div>
      ))}
      <button
        type="button"
        class="btn btn-secondary"
        onClick={() => update([...rows, emptyRow(subFields)])}
      >
        ＋ Add {humanize(field.name).replace(/s$/, "")}
      </button>
    </div>
  );
}
```

- [ ] **Step 4: Array-aware config in `MonitorForm.tsx`.** Widen `FieldValue` and update the three functions so List fields hold/coerce arrays:

```ts
// A list field's value is an array of coerced sub-objects.
type FieldValue = string | boolean | Record<string, unknown>[];
```

In `initialFieldValue`, handle the list kind (return the existing array, or `[]`):

```ts
function initialFieldValue(field: Field, existing: unknown): FieldValue {
  if (field.kind === "list") {
    return Array.isArray(existing) ? (existing as Record<string, unknown>[]) : [];
  }
  const raw = existing !== undefined ? existing : field.default;
  if (field.kind === "bool") return Boolean(raw);
  if (raw === null || raw === undefined) return "";
  return String(raw);
}
```

In `coerceFieldValue`, coerce each row's sub-fields (omitting null-coerced optionals, matching the existing scalar behavior):

```ts
function coerceFieldValue(field: Field, raw: FieldValue | undefined): unknown {
  if (field.kind === "list") {
    const rows = Array.isArray(raw) ? raw : [];
    return rows.map((row) => {
      const out: Record<string, unknown> = {};
      for (const sf of field.fields ?? []) {
        const c = coerceFieldValue(sf, row[sf.name] as FieldValue);
        if (c !== null) out[sf.name] = c;
      }
      return out;
    });
  }
  if (field.kind === "bool") return Boolean(raw);
  const str = typeof raw === "string" ? raw.trim() : "";
  if (str === "") return null;
  if (field.kind === "int") { const n = parseInt(str, 10); return Number.isFinite(n) ? n : null; }
  if (field.kind === "float") { const n = parseFloat(str); return Number.isFinite(n) ? n : null; }
  return str;
}
```

In `handleSubmit`, keep a List field even when it coerces to an empty array (so `rules: []` is sent and the backend reports the "no rules" Unknown), and skip the scalar required-emptiness check for list kind:

```ts
for (const field of fields) {
  const coerced = coerceFieldValue(field, configValues[field.name]);
  if (field.kind === "list") {
    config[field.name] = coerced;      // always include (may be [])
    continue;
  }
  if (coerced !== null) config[field.name] = coerced;
  if (field.required && (coerced === null || coerced === "")) missing.push(humanize(field.name));
}
```

- [ ] **Step 5: Minimal row styling** in `ui/src/styles.css`:

```css
.list-row { display: flex; flex-wrap: wrap; gap: 0.5rem; align-items: flex-end; margin-bottom: 0.5rem; }
.list-row .form-field { flex: 1 1 8rem; margin: 0; }
.list-row-remove { flex: 0 0 auto; }
```

- [ ] **Step 6: Build the UI and verify it compiles/type-checks**

Run: `npm --prefix ui run build 2>&1 | tail -20`
Expected: build succeeds with no TypeScript errors.

- [ ] **Step 7: Browser verification.** Start the dev server against a local daemon, add a `prometheus` monitor with two rules via the builder, save, and confirm it round-trips (reopen edit → rules present). Follow the same live-verification approach used for the other checks (Vite dev proxy or a `cargo run` daemon + built UI). Confirm: the `op` field renders as a dropdown; ＋ Add rule / ✕ remove work; saving stores `config.rules` as an array; editing repopulates the rows.

- [ ] **Step 8: Commit**

```bash
git add ui/src/types.ts ui/src/components/ListField.tsx ui/src/components/SchemaField.tsx ui/src/components/MonitorForm.tsx ui/src/styles.css
git commit -m "feat: UI list-of-objects field kind and options dropdowns

SchemaField renders options as a select and delegates list fields to a new
ListField (repeatable rows built from sub-fields). MonitorForm initializes,
coerces, and submits array-valued config, so prometheus rules can be authored
in the add/edit modal."
```

---

### Task 6: UI — metric/label autocomplete via the inspect endpoint

Adds a "Fetch metrics" action to the prometheus rule builder that calls the inspect endpoint and suggests real metric names and label matchers.

**Files:**
- Modify: `ui/src/api.ts` (client method + response types)
- Modify: `ui/src/types.ts` (inspect response types)
- Modify: `ui/src/components/MonitorForm.tsx` (fetch button + pass suggestions down)
- Modify: `ui/src/components/ListField.tsx` (render `<datalist>` suggestions for text sub-inputs)

**Interfaces:**
- Consumes: `POST /api/v1/checks/prometheus/inspect` (Task 4).
- Produces: `api.inspectPrometheus(url, timeoutSecs?)` and a `suggest` mechanism from `MonitorForm` → `ListField`.

- [ ] **Step 1: Inspect types** in `ui/src/types.ts`:

```ts
export interface PrometheusInspect {
  metrics: Record<string, { labels: Record<string, string[]> }>;
}
```

- [ ] **Step 2: API client method** in `ui/src/api.ts`:

```ts
import type { /* …existing…, */ PrometheusInspect } from "./types";

// inside ApiClient:
inspectPrometheus(url: string, timeoutSecs?: number): Promise<PrometheusInspect> {
  return request<PrometheusInspect>("/checks/prometheus/inspect", {
    method: "POST",
    body: JSON.stringify({ url, timeout_secs: timeoutSecs }),
  });
}
```

- [ ] **Step 3: Fetch button + suggestions in `MonitorForm.tsx`.** When the selected type is `prometheus`, render a "Fetch metrics" button (enabled once `url` is non-empty) that calls `api.inspectPrometheus`, stores the result in state, and derives a `suggest(subFieldName, row)` callback:
  - `metric` → `Object.keys(inspect.metrics)`
  - `labels` → for the row's chosen `metric`, flatten its label map into `key="value"` strings (e.g. `task_type="backup"`), plus bare `key=""` stubs.

Pass an optional `suggest?: (subFieldName: string, row: Record<string, unknown>) => string[]` prop into the rules `SchemaField`/`ListField`. Show a small status line ("Fetched 6 metrics" / the error message returned by the 502) next to the button. Keep this behind a `typeId === "prometheus"` check so other checks are unaffected.

- [ ] **Step 4: `<datalist>` in `ListField.tsx`.** Accept the optional `suggest` prop; for each text sub-input, if `suggest` returns values, attach a `list={datalistId}` and render a `<datalist>` with those `<option>`s. (Preact/HTML `datalist` gives free-typing + suggestions, which fits both exact metric names and partial label matchers.) Requires threading `suggest` from `SchemaField`'s list branch into `ListField`; for non-list callers it's simply absent.

- [ ] **Step 5: Build + type-check**

Run: `npm --prefix ui run build 2>&1 | tail -20`
Expected: clean build.

- [ ] **Step 6: Browser verification against the real endpoint.** With a daemon running, add a `prometheus` monitor, set `url` to a reachable metrics endpoint (e.g. `http://backups.home/metrics` on the user's network), click **Fetch metrics**, and confirm: the `metric` field suggests real names; after choosing one, the `labels` field suggests that metric's `key="value"` pairs; a rule saved this way runs green/red as expected via "Run now". Confirm an unreachable URL surfaces the 502 message inline rather than throwing.

- [ ] **Step 7: Commit**

```bash
git add ui/src/api.ts ui/src/types.ts ui/src/components/MonitorForm.tsx ui/src/components/ListField.tsx
git commit -m "feat: metric/label autocomplete in the prometheus rule builder

A Fetch metrics button calls the inspect endpoint and drives datalist
suggestions: metric names, then that metric's label matchers. Errors surface
inline. Scoped to the prometheus check; other checks are unaffected."
```

---

## Self-Review

**Spec coverage:**
- New `prometheus` check type (fetch→parse→evaluate) → Tasks 2–3. ✅
- Rule model (metric/labels/op/threshold/critical; one component per series; critical/non-critical→Critical/Degraded; zero-match→Unknown) → Task 3 `evaluate` + tests. ✅
- Config schema (url/timeout/rules-list; op options) → Task 3 `schema()`; List/options primitives → Task 1. ✅
- Error handling table (unreachable/non-2xx→Critical, timeout→Unknown, unparseable→Unknown, empty rules→Unknown, bad matcher→Unknown) → Task 2 `fetch_and_parse` + Task 3 `map_scrape_error`/`evaluate` + tests. ✅
- Inspect endpoint → Task 4. ✅
- Schema/UI extensions (List kind, options, form array handling) → Tasks 1 & 5. ✅
- Autocomplete → Task 6. ✅
- `prometheus-parse` dependency via `cargo add` → Task 2. ✅
- `with_builtins` count 6→7 → Task 3. ✅

**Placeholder scan:** No TBD/TODO; every code and test step carries concrete content. The two "adjust if the crate's API differs" notes (Task 2 Step 1, Task 3 Step 7) are explicit verification instructions with the exact fallback, not deferrals.

**Type consistency:** `Series`, `Op`, `Rule`, `PrometheusConfig`, `ScrapeError`, `fetch_and_parse`, `evaluate`, `inspect_result`, `InspectResult`/`MetricInfo`, `PrometheusCheck` are defined once (Tasks 2–4) and referenced with matching names/signatures in later tasks and in `api.rs`. The UI `Field` type (Task 5) matches the Rust schema's serialized shape (`kind:"list"`, `options`, `fields`), and `PrometheusInspect` (Task 6) matches `InspectResult`'s JSON (`metrics → {labels: {key: [values]}}`).

**Note for the implementer:** the frontend tasks (5–6) are verified in the browser rather than by automated unit tests, matching this repo's established practice for UI work; the backend tasks are fully TDD with `cargo test`.
