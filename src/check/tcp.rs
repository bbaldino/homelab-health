use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::CheckReport;
use crate::status::Status;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::net::TcpStream;

#[derive(Deserialize)]
struct TcpConfig {
    host: String,
    port: u16,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_timeout() -> u64 {
    5
}

pub struct TcpCheck;

#[async_trait]
impl CheckType for TcpCheck {
    fn type_id(&self) -> &'static str {
        "tcp"
    }

    fn schema(&self) -> ConfigSchema {
        ConfigSchema {
            fields: vec![
                Field {
                    name: "host",
                    kind: FieldKind::String,
                    required: true,
                    default: None,
                    help: "Hostname or IP to connect to",
                },
                Field {
                    name: "port",
                    kind: FieldKind::Int,
                    required: true,
                    default: None,
                    help: "TCP port that should accept connections",
                },
                Field {
                    name: "timeout_secs",
                    kind: FieldKind::Int,
                    required: false,
                    default: Some(json!(5)),
                    help: "Connect timeout in seconds",
                },
            ],
        }
    }

    async fn run(&self, cfg: &Value) -> CheckReport {
        let cfg: TcpConfig = match serde_json::from_value(cfg.clone()) {
            Ok(c) => c,
            Err(e) => return CheckReport::new(Status::Unknown, format!("bad config: {e}")),
        };

        let addr = format!("{}:{}", cfg.host, cfg.port);
        let connect = TcpStream::connect(&addr);
        match tokio::time::timeout(Duration::from_secs(cfg.timeout_secs), connect).await {
            Ok(Ok(_)) => CheckReport::ok(format!("{addr} accepting connections")),
            Ok(Err(e)) => CheckReport::new(Status::Critical, format!("connect failed: {e}")),
            Err(_) => CheckReport::new(Status::Critical, format!("connect timed out: {addr}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn ok_when_port_accepts() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Keep accepting in the background so the connect succeeds.
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let cfg = json!({ "host": "127.0.0.1", "port": addr.port() });
        let report = TcpCheck.run(&cfg).await;
        assert_eq!(report.status, Status::Ok);
    }

    #[tokio::test]
    async fn critical_when_port_closed() {
        // Bind then drop to get a definitely-closed port.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let cfg = json!({ "host": "127.0.0.1", "port": port, "timeout_secs": 1 });
        let report = TcpCheck.run(&cfg).await;
        assert_eq!(report.status, Status::Critical);
    }

    #[tokio::test]
    async fn unknown_when_config_invalid() {
        let report = TcpCheck.run(&json!({ "host": "x" })).await;
        assert_eq!(report.status, Status::Unknown);
    }
}
