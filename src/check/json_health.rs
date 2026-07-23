use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::{CheckReport, Component};
use crate::status::Status;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonHealthConfig {
    url: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_timeout() -> u64 {
    10
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
                },
                Field {
                    name: "timeout_secs",
                    kind: FieldKind::Int,
                    required: false,
                    default: Some(json!(10)),
                    help: "Request timeout in seconds",
                    secret: false,
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
}
