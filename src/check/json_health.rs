use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::{CheckReport, Component};
use crate::status::Status;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonHealthConfig {
    url: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default)]
    field_rules: Vec<FieldRule>,
}

fn default_timeout() -> u64 {
    10
}

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

#[allow(clippy::derivable_impls)] // kept as an explicit impl per spec
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

/// A service's self-reported status: strictly ok/degraded/critical. A value
/// outside this set (including "unknown") fails deserialization, which the
/// caller turns into a check-level Unknown.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ServiceStatus {
    Ok,
    Degraded,
    Critical,
}

impl From<ServiceStatus> for Status {
    fn from(s: ServiceStatus) -> Status {
        match s {
            ServiceStatus::Ok => Status::Ok,
            ServiceStatus::Degraded => Status::Degraded,
            ServiceStatus::Critical => Status::Critical,
        }
    }
}

#[derive(Deserialize)]
struct HealthComponent {
    name: String,
    status: ServiceStatus,
    critical: bool,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct HealthBody {
    #[serde(default)]
    status: Option<ServiceStatus>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    components: Vec<HealthComponent>,
}

pub struct JsonHealthCheck;

/// Ensure a non-ok status carries a non-empty message (the Component/CheckReport
/// invariant), falling back to the status name when the service left it blank.
fn ensure_message(status: Status, message: String) -> String {
    if !message.is_empty() || status == Status::Ok {
        message
    } else {
        format!("{status:?}")
    }
}

impl JsonHealthCheck {
    /// Pure mapping from a parsed body + field-rule components to a CheckReport.
    fn evaluate(body: HealthBody, field_components: Vec<Component>) -> CheckReport {
        let mut components: Vec<Component> = body
            .components
            .into_iter()
            .map(|c| {
                let status = Status::from(c.status);
                Component::new(
                    c.name,
                    status,
                    c.critical,
                    ensure_message(status, c.message),
                )
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
                None => CheckReport::new(
                    Status::Unknown,
                    "health body had neither status nor components",
                ),
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
}

#[async_trait]
impl CheckType for JsonHealthCheck {
    fn type_id(&self) -> &'static str {
        "json-health"
    }

    fn schema(&self) -> ConfigSchema {
        ConfigSchema {
            fields: vec![
                Field {
                    name: "url",
                    kind: FieldKind::String,
                    required: true,
                    default: None,
                    help: "URL of the service's JSON /health endpoint",
                    secret: false,
                    options: None,
                    fields: None,
                },
                Field {
                    name: "timeout_secs",
                    kind: FieldKind::Int,
                    required: false,
                    default: Some(json!(10)),
                    help: "Request timeout in seconds",
                    secret: false,
                    options: None,
                    fields: None,
                },
                Field {
                    name: "field_rules",
                    kind: FieldKind::List,
                    required: false,
                    default: None,
                    help: "Threshold rules over fields in the JSON body",
                    secret: false,
                    options: None,
                    fields: Some(vec![
                        Field {
                            name: "name",
                            kind: FieldKind::String,
                            required: true,
                            default: None,
                            help: "Component name",
                            secret: false,
                            options: None,
                            fields: None,
                        },
                        Field {
                            name: "field",
                            kind: FieldKind::String,
                            required: true,
                            default: None,
                            help: "JSON field path, e.g. access_token_expires_at (dotted for nested)",
                            secret: false,
                            options: None,
                            fields: None,
                        },
                        Field {
                            name: "interpret",
                            kind: FieldKind::String,
                            required: true,
                            default: None,
                            help: "How to read the field",
                            secret: false,
                            options: Some(vec![json!("timestamp"), json!("number")]),
                            fields: None,
                        },
                        Field {
                            name: "op",
                            kind: FieldKind::String,
                            required: false,
                            default: Some(json!("<")),
                            help: "Comparison (worse when value crosses the threshold)",
                            secret: false,
                            options: Some(vec![json!("<"), json!(">")]),
                            fields: None,
                        },
                        Field {
                            name: "degraded",
                            kind: FieldKind::Float,
                            required: false,
                            default: None,
                            help: "Degraded threshold (for timestamp: seconds remaining, e.g. 3600 = 1h)",
                            secret: false,
                            options: None,
                            fields: None,
                        },
                        Field {
                            name: "critical",
                            kind: FieldKind::Float,
                            required: false,
                            default: None,
                            help: "Critical threshold (for timestamp: seconds remaining, e.g. 600 = 10m)",
                            secret: false,
                            options: None,
                            fields: None,
                        },
                    ]),
                },
            ],
        }
    }

    async fn run(&self, cfg: &Value) -> CheckReport {
        let cfg: JsonHealthConfig = match serde_json::from_value(cfg.clone()) {
            Ok(c) => c,
            Err(e) => return CheckReport::new(Status::Unknown, format!("bad config: {e}")),
        };

        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()
        {
            Ok(c) => c,
            Err(e) => return CheckReport::new(Status::Unknown, format!("client error: {e}")),
        };

        let resp = match client.get(&cfg.url).send().await {
            Ok(r) => r,
            Err(e) => return CheckReport::new(Status::Unknown, format!("request failed: {e}")),
        };

        // Captured before the body is consumed, and reported only when decoding
        // fails. A decode failure alone cannot distinguish "the service
        // returned garbage" from "a reverse proxy in front of it returned an
        // HTML error page because it could not reach the service" — the status
        // is the only thing in the response that separates them.
        let status = resp.status();

        // Parse once as a raw Value so field rules can read arbitrary fields,
        // then map the contract shape from it. Parse regardless of HTTP status
        // (a 503-on-critical service still has a readable body per the health
        // contract), so the status is NOT used to reject a response — only to
        // explain one that failed to decode.
        let value: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                return CheckReport::new(
                    Status::Unknown,
                    format!("invalid health body (HTTP {status}): {e}"),
                );
            }
        };
        let body: HealthBody = match serde_json::from_value(value.clone()) {
            Ok(b) => b,
            Err(e) => {
                return CheckReport::new(
                    Status::Unknown,
                    format!("invalid health body (HTTP {status}): {e}"),
                );
            }
        };

        let field_components = evaluate_field_rules(&value, &cfg.field_rules, chrono::Utc::now());
        JsonHealthCheck::evaluate(body, field_components)
    }
}

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
            let unknown =
                |msg: String| Component::new(rule.name.clone(), Status::Unknown, true, msg);

            if rule.degraded.is_none() && rule.critical.is_none() {
                return unknown(format!("rule '{}' has no thresholds", rule.name));
            }
            let raw = match read_path(body, &rule.field) {
                Some(v) => v,
                None => return unknown(format!("field '{}' not found", rule.field)),
            };

            // interpret → (numeric value, message-friendly rendering)
            let (value, render): (f64, String) = match rule.interpret {
                Interpret::Timestamp => match raw
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                {
                    Some(dt) => {
                        let secs = (dt.with_timezone(&Utc) - now).num_seconds() as f64;
                        (secs, humanize_remaining(secs))
                    }
                    None => {
                        return unknown(format!(
                            "field '{}' is not an RFC3339 timestamp",
                            rule.field
                        ));
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn parse(v: Value) -> HealthBody {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn critical_critical_component_makes_report_critical() {
        let report = JsonHealthCheck::evaluate(
            parse(json!({
                "components": [
                    { "name": "database", "status": "critical", "critical": true, "message": "conn refused" }
                ]
            })),
            vec![],
        );
        assert_eq!(report.status, Status::Critical);
        assert!(report.message.contains("database"));
    }

    #[test]
    fn noncritical_critical_component_caps_at_degraded() {
        let report = JsonHealthCheck::evaluate(
            parse(json!({
                "components": [
                    { "name": "spotify", "status": "critical", "critical": false, "message": "token refresh failing" }
                ]
            })),
            vec![],
        );
        assert_eq!(report.status, Status::Degraded);
    }

    #[test]
    fn status_only_no_components() {
        let report = JsonHealthCheck::evaluate(parse(json!({ "status": "ok" })), vec![]);
        assert_eq!(report.status, Status::Ok);
    }

    #[test]
    fn empty_body_is_unknown() {
        let report = JsonHealthCheck::evaluate(parse(json!({})), vec![]);
        assert_eq!(report.status, Status::Unknown);
    }

    #[test]
    fn non_ok_component_missing_message_gets_fallback() {
        // Service violates the contract by omitting message on a non-ok component;
        // we must not panic (Component::new debug-asserts non-empty message).
        let report = JsonHealthCheck::evaluate(
            parse(json!({
                "components": [ { "name": "x", "status": "critical", "critical": true } ]
            })),
            vec![],
        );
        assert_eq!(report.status, Status::Critical);
        assert_eq!(report.components[0].message, "Critical");
    }

    #[tokio::test]
    async fn fetches_healthy_body_over_http() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "ok" })))
            .mount(&server)
            .await;
        let report = JsonHealthCheck.run(&json!({ "url": server.uri() })).await;
        assert_eq!(report.status, Status::Ok);
    }

    #[tokio::test]
    async fn parses_body_even_on_503() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({
                "status": "critical", "message": "datastore down"
            })))
            .mount(&server)
            .await;
        let report = JsonHealthCheck.run(&json!({ "url": server.uri() })).await;
        assert_eq!(report.status, Status::Critical);
        assert_eq!(report.message, "datastore down");
    }

    #[tokio::test]
    async fn non_json_body_reports_the_http_status() {
        // A reverse proxy's 502 page is HTML, not JSON. Without the status in
        // the message, "invalid health body" reads as "the service returned
        // garbage" — but a 502 means the proxy could not reach the service at
        // all. Those are different faults in different places, and the status
        // is the only thing in the response that tells them apart.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(502)
                    .set_body_string("<html><head><title>502 Bad Gateway</title></head></html>")
                    .insert_header("content-type", "text/html"),
            )
            .mount(&server)
            .await;
        let report = JsonHealthCheck.run(&json!({ "url": server.uri() })).await;
        assert_eq!(report.status, Status::Unknown);
        assert!(
            report.message.contains("502"),
            "message must carry the HTTP status, got: {}",
            report.message
        );
    }

    #[tokio::test]
    async fn wrong_shaped_json_reports_the_http_status() {
        // Valid JSON whose types don't match the contract. Note every
        // HealthBody field is #[serde(default)], so a merely *unfamiliar*
        // object like {"nope": 1} deserializes fine and is caught later by the
        // neither-status-nor-components check — reaching this path takes a real
        // type mismatch. The status is less diagnostic here than for a proxy
        // error, but carrying it keeps both decode failures reading the same.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "components": "not-an-array" })),
            )
            .mount(&server)
            .await;
        let report = JsonHealthCheck.run(&json!({ "url": server.uri() })).await;
        assert_eq!(report.status, Status::Unknown);
        assert!(
            report.message.contains("200"),
            "message must carry the HTTP status, got: {}",
            report.message
        );
    }

    #[tokio::test]
    async fn invalid_service_status_is_unknown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "weird" })))
            .mount(&server)
            .await;
        let report = JsonHealthCheck.run(&json!({ "url": server.uri() })).await;
        assert_eq!(report.status, Status::Unknown);
    }

    #[tokio::test]
    async fn unreachable_is_unknown() {
        let report = JsonHealthCheck
            .run(&json!({ "url": "http://127.0.0.1:1/health", "timeout_secs": 1 }))
            .await;
        assert_eq!(report.status, Status::Unknown);
    }

    #[tokio::test]
    async fn bad_config_is_unknown() {
        // unknown field rejected by deny_unknown_fields
        let report = JsonHealthCheck
            .run(&json!({ "url": "http://x", "bogus": 1 }))
            .await;
        assert_eq!(report.status, Status::Unknown);
    }

    #[tokio::test]
    async fn run_appends_field_rule_component() {
        let exp = (chrono::Utc::now() + chrono::Duration::minutes(20)).to_rfc3339();
        let body = json!({
            "status": "ok", "message": "",
            "components": [{ "name": "credentials", "status": "ok", "critical": true, "message": "" }],
            "access_token_expires_at": exp
        })
        .to_string();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let report = JsonHealthCheck
            .run(&json!({
                "url": server.uri(),
                "field_rules": [{ "name": "access_token", "field": "access_token_expires_at",
                    "interpret": "timestamp", "degraded": 3600, "critical": 600 }]
            }))
            .await;
        assert_eq!(report.status, Status::Degraded);
        assert!(
            report
                .components
                .iter()
                .any(|c| c.name == "access_token" && c.status == Status::Degraded)
        );
        assert!(report.components.iter().any(|c| c.name == "credentials"));
    }

    #[tokio::test]
    async fn run_far_off_token_stays_ok() {
        let exp = (chrono::Utc::now() + chrono::Duration::hours(3)).to_rfc3339();
        let body =
            json!({ "status": "ok", "components": [], "access_token_expires_at": exp }).to_string();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let report = JsonHealthCheck
            .run(&json!({
                "url": server.uri(),
                "field_rules": [{ "name": "access_token", "field": "access_token_expires_at",
                    "interpret": "timestamp", "degraded": 3600, "critical": 600 }]
            }))
            .await;
        assert_eq!(report.status, Status::Ok);
    }

    #[test]
    fn schema_exposes_field_rules_list() {
        let s = JsonHealthCheck.schema();
        let fr = s.fields.iter().find(|f| f.name == "field_rules").unwrap();
        assert!(matches!(fr.kind, FieldKind::List));
        let sub = fr.fields.as_ref().unwrap();
        let interp = sub.iter().find(|f| f.name == "interpret").unwrap();
        assert!(
            interp
                .options
                .as_ref()
                .unwrap()
                .contains(&json!("timestamp"))
        );
    }

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
        }))
        .unwrap();
        assert_eq!(cfg.field_rules.len(), 1);
        let r = &cfg.field_rules[0];
        assert!(matches!(r.interpret, Interpret::Timestamp));
        assert!(matches!(r.op, Op::Lt)); // default
        assert_eq!(r.critical, Some(600.0));
    }

    #[test]
    fn config_still_rejects_unknown_and_defaults_empty_rules() {
        let empty: JsonHealthConfig = serde_json::from_value(json!({ "url": "http://x" })).unwrap();
        assert!(empty.field_rules.is_empty());
        assert!(
            serde_json::from_value::<JsonHealthConfig>(json!({ "url": "http://x", "bogus": 1 }))
                .is_err()
        );
        // op parses the symbol form
        let r: FieldRule = serde_json::from_value(json!({
            "name": "n", "field": "f", "interpret": "number", "op": ">", "degraded": 80
        }))
        .unwrap();
        assert!(matches!(r.op, Op::Gt));
    }

    fn at(s: &str) -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ts_rule(deg: f64, crit: f64) -> FieldRule {
        FieldRule {
            name: "tok".into(),
            field: "exp".into(),
            interpret: Interpret::Timestamp,
            op: Op::Lt,
            degraded: Some(deg),
            critical: Some(crit),
        }
    }

    #[test]
    fn timestamp_far_out_is_ok() {
        let now = at("2026-08-01T00:00:00Z");
        let body = json!({ "exp": "2026-08-01T05:00:00Z" }); // 5h out
        let c = evaluate_field_rules(&body, &[ts_rule(3600.0, 600.0)], now);
        assert_eq!(c[0].status, Status::Ok);
        assert!(c[0].critical);
    }

    #[test]
    fn timestamp_within_degraded_and_critical() {
        let now = at("2026-08-01T00:00:00Z");
        let deg = json!({ "exp": "2026-08-01T00:30:00Z" }); // 30m out → < 3600
        assert_eq!(
            evaluate_field_rules(&deg, &[ts_rule(3600.0, 600.0)], now)[0].status,
            Status::Degraded
        );
        let crit = json!({ "exp": "2026-08-01T00:05:00Z" }); // 5m out → < 600
        assert_eq!(
            evaluate_field_rules(&crit, &[ts_rule(3600.0, 600.0)], now)[0].status,
            Status::Critical
        );
    }

    #[test]
    fn expired_timestamp_is_critical() {
        let now = at("2026-08-01T00:00:00Z");
        let body = json!({ "exp": "2026-07-31T23:59:00Z" }); // 1m ago
        assert_eq!(
            evaluate_field_rules(&body, &[ts_rule(3600.0, 600.0)], now)[0].status,
            Status::Critical
        );
    }

    #[test]
    fn number_lt_and_gt() {
        let now = at("2026-08-01T00:00:00Z");
        let lt = FieldRule {
            name: "n".into(),
            field: "v".into(),
            interpret: Interpret::Number,
            op: Op::Lt,
            degraded: Some(100.0),
            critical: Some(10.0),
        };
        assert_eq!(
            evaluate_field_rules(&json!({"v": 5}), std::slice::from_ref(&lt), now)[0].status,
            Status::Critical
        );
        assert_eq!(
            evaluate_field_rules(&json!({"v": 50}), std::slice::from_ref(&lt), now)[0].status,
            Status::Degraded
        );
        assert_eq!(
            evaluate_field_rules(&json!({"v": 500}), &[lt], now)[0].status,
            Status::Ok
        );
        let gt = FieldRule {
            name: "n".into(),
            field: "v".into(),
            interpret: Interpret::Number,
            op: Op::Gt,
            degraded: Some(80.0),
            critical: Some(95.0),
        };
        assert_eq!(
            evaluate_field_rules(&json!({"v": 99}), &[gt], now)[0].status,
            Status::Critical
        );
    }

    #[test]
    fn warn_only_rule_never_criticals() {
        let now = at("2026-08-01T00:00:00Z");
        let r = FieldRule {
            name: "tok".into(),
            field: "exp".into(),
            interpret: Interpret::Timestamp,
            op: Op::Lt,
            degraded: Some(3600.0),
            critical: None,
        };
        let body = json!({ "exp": "2026-07-31T00:00:00Z" }); // long expired
        assert_eq!(
            evaluate_field_rules(&body, &[r], now)[0].status,
            Status::Degraded
        );
    }

    #[test]
    fn missing_bad_and_no_threshold_are_unknown() {
        let now = at("2026-08-01T00:00:00Z");
        // missing field
        assert_eq!(
            evaluate_field_rules(&json!({}), &[ts_rule(3600.0, 600.0)], now)[0].status,
            Status::Unknown
        );
        // timestamp interpret but not a valid timestamp
        assert_eq!(
            evaluate_field_rules(&json!({"exp": "nope"}), &[ts_rule(3600.0, 600.0)], now)[0].status,
            Status::Unknown
        );
        // number interpret but not a number
        let numr = FieldRule {
            name: "n".into(),
            field: "v".into(),
            interpret: Interpret::Number,
            op: Op::Lt,
            degraded: Some(1.0),
            critical: None,
        };
        assert_eq!(
            evaluate_field_rules(&json!({"v": "x"}), &[numr], now)[0].status,
            Status::Unknown
        );
        // no thresholds set
        let none = FieldRule {
            name: "n".into(),
            field: "v".into(),
            interpret: Interpret::Number,
            op: Op::Lt,
            degraded: None,
            critical: None,
        };
        assert_eq!(
            evaluate_field_rules(&json!({"v": 1}), &[none], now)[0].status,
            Status::Unknown
        );
    }
}
