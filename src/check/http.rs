use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::CheckReport;
use crate::status::Status;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpConfig {
    url: String,
    #[serde(default = "default_status")]
    expected_status: u16,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_status() -> u16 {
    200
}
fn default_timeout() -> u64 {
    10
}

pub struct HttpCheck;

#[async_trait]
impl CheckType for HttpCheck {
    fn type_id(&self) -> &'static str {
        "http"
    }

    fn schema(&self) -> ConfigSchema {
        ConfigSchema {
            fields: vec![
                Field {
                    name: "url",
                    kind: FieldKind::String,
                    required: true,
                    default: None,
                    help: "URL to request (GET)",
                    secret: false,
                },
                Field {
                    name: "expected_status",
                    kind: FieldKind::Int,
                    required: false,
                    default: Some(json!(200)),
                    help: "HTTP status code that means healthy",
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
        let cfg: HttpConfig = match serde_json::from_value(cfg.clone()) {
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

        match client.get(&cfg.url).send().await {
            Ok(resp) => {
                let got = resp.status().as_u16();
                if got == cfg.expected_status {
                    CheckReport::ok(format!("HTTP {got}"))
                } else {
                    CheckReport::new(
                        Status::Critical,
                        format!("HTTP {got}, expected {}", cfg.expected_status),
                    )
                }
            }
            Err(e) => CheckReport::new(Status::Unknown, format!("request failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn ok_when_status_matches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let cfg = json!({ "url": format!("{}/health", server.uri()) });
        let report = HttpCheck.run(&cfg).await;
        assert_eq!(report.status, Status::Ok);
    }

    #[tokio::test]
    async fn critical_when_status_mismatches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let cfg = json!({ "url": server.uri() });
        let report = HttpCheck.run(&cfg).await;
        assert_eq!(report.status, Status::Critical);
        assert!(report.message.contains("503"));
    }

    #[tokio::test]
    async fn unknown_when_config_invalid() {
        let report = HttpCheck.run(&json!({})).await;
        assert_eq!(report.status, Status::Unknown);
    }

    #[tokio::test]
    async fn unknown_when_unreachable() {
        // Port 1 is not listening; connection fails fast.
        let cfg = json!({ "url": "http://127.0.0.1:1/", "timeout_secs": 1 });
        let report = HttpCheck.run(&cfg).await;
        assert_eq!(report.status, Status::Unknown);
    }

    #[tokio::test]
    async fn unknown_config_field_is_unknown() {
        // A typo'd/unexpected field must fail deserialization, not run with a default.
        let cfg = json!({ "url": "http://127.0.0.1:1/", "timeout_sec": 5 });
        let report = HttpCheck.run(&cfg).await;
        assert_eq!(report.status, Status::Unknown);
    }
}
