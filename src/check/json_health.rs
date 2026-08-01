use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::{CheckReport, Component};
use crate::status::Status;
use async_trait::async_trait;
#[allow(unused_imports)] // DateTime/Utc are used starting in Task 2
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
    #[allow(dead_code)] // read starting in Task 3 (evaluate_field_rules)
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
#[allow(dead_code)] // called starting in Task 3 (evaluate_field_rules)
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
    /// Pure mapping from a parsed body to a CheckReport (hermetic-testable).
    fn evaluate(body: HealthBody) -> CheckReport {
        if !body.components.is_empty() {
            let components = body
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
            return CheckReport::from_components(components);
        }
        match body.status {
            Some(s) => {
                let status = Status::from(s);
                CheckReport::new(status, ensure_message(status, body.message))
            }
            None => CheckReport::new(
                Status::Unknown,
                "health body had neither status nor components",
            ),
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

        // Parse the body regardless of HTTP status code (a 503-on-critical
        // service still has a readable body per the health contract).
        let body: HealthBody = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                return CheckReport::new(Status::Unknown, format!("invalid health body: {e}"));
            }
        };

        JsonHealthCheck::evaluate(body)
    }
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
        let report = JsonHealthCheck::evaluate(parse(json!({
            "components": [
                { "name": "database", "status": "critical", "critical": true, "message": "conn refused" }
            ]
        })));
        assert_eq!(report.status, Status::Critical);
        assert!(report.message.contains("database"));
    }

    #[test]
    fn noncritical_critical_component_caps_at_degraded() {
        let report = JsonHealthCheck::evaluate(parse(json!({
            "components": [
                { "name": "spotify", "status": "critical", "critical": false, "message": "token refresh failing" }
            ]
        })));
        assert_eq!(report.status, Status::Degraded);
    }

    #[test]
    fn status_only_no_components() {
        let report = JsonHealthCheck::evaluate(parse(json!({ "status": "ok" })));
        assert_eq!(report.status, Status::Ok);
    }

    #[test]
    fn empty_body_is_unknown() {
        let report = JsonHealthCheck::evaluate(parse(json!({})));
        assert_eq!(report.status, Status::Unknown);
    }

    #[test]
    fn non_ok_component_missing_message_gets_fallback() {
        // Service violates the contract by omitting message on a non-ok component;
        // we must not panic (Component::new debug-asserts non-empty message).
        let report = JsonHealthCheck::evaluate(parse(json!({
            "components": [ { "name": "x", "status": "critical", "critical": true } ]
        })));
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
}
