use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::{CheckReport, Component};
use crate::status::Status;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

const QUERY: &str = "{ array { state capacity { kilobytes { free total } } \
disks { name status temp numErrors } parities { name status temp numErrors } \
caches { name status temp numErrors } } }";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnraidConfig {
    url: String,
    api_key: String,
    #[serde(default = "default_warn")]
    free_space_warn_pct: f64,
    #[serde(default = "default_crit")]
    free_space_critical_pct: f64,
    #[serde(default = "default_temp")]
    disk_temp_warn_c: i64,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_warn() -> f64 {
    10.0
}
fn default_crit() -> f64 {
    3.0
}
fn default_temp() -> i64 {
    55
}
fn default_timeout() -> u64 {
    10
}

#[derive(Deserialize)]
struct GqlResponse {
    data: Option<GqlData>,
}
#[derive(Deserialize)]
struct GqlData {
    array: ArrayData,
}
#[derive(Deserialize)]
struct ArrayData {
    state: String,
    capacity: Capacity,
    #[serde(default)]
    disks: Vec<Disk>,
    #[serde(default)]
    parities: Vec<Disk>,
    #[serde(default)]
    caches: Vec<Disk>,
}
#[derive(Deserialize)]
struct Capacity {
    kilobytes: Kilobytes,
}
#[derive(Deserialize)]
struct Kilobytes {
    free: String,
    total: String,
}
#[derive(Deserialize)]
struct Disk {
    name: String,
    status: String,
    #[serde(default)]
    temp: Option<i64>,
    #[serde(rename = "numErrors", default)]
    num_errors: i64,
}

pub struct UnraidCheck;

/// Thresholds passed to evaluate (from config).
struct Thresholds {
    warn_pct: f64,
    crit_pct: f64,
    temp_warn: i64,
}

impl UnraidCheck {
    fn disk_component(d: &Disk, critical: bool, temp_warn: i64) -> Component {
        let (status, message) = if d.status != "DISK_OK" {
            (Status::Critical, d.status.clone())
        } else if d.num_errors > 0 {
            (Status::Degraded, format!("{} errors", d.num_errors))
        } else if d.temp.is_some_and(|t| t >= temp_warn) {
            (Status::Degraded, format!("{}°C (hot)", d.temp.unwrap()))
        } else {
            let msg = d
                .temp
                .map_or_else(|| "ok".to_string(), |t| format!("{t}°C"));
            (Status::Ok, msg)
        };
        Component::new(&d.name, status, critical, message)
    }

    fn evaluate(array: ArrayData, t: &Thresholds) -> CheckReport {
        let mut components = Vec::new();

        if array.state == "STARTED" {
            components.push(Component::new("array", Status::Ok, true, "started"));
        } else {
            components.push(Component::new(
                "array",
                Status::Critical,
                true,
                format!("array {}", array.state),
            ));
        }

        for d in &array.parities {
            components.push(Self::disk_component(d, true, t.temp_warn));
        }
        for d in &array.disks {
            components.push(Self::disk_component(d, true, t.temp_warn));
        }
        for d in &array.caches {
            components.push(Self::disk_component(d, false, t.temp_warn));
        }

        // free space
        let free = array.capacity.kilobytes.free.parse::<f64>().unwrap_or(0.0);
        let total = array.capacity.kilobytes.total.parse::<f64>().unwrap_or(0.0);
        if total <= 0.0 {
            components.push(Component::new(
                "free space",
                Status::Unknown,
                false,
                "capacity unavailable",
            ));
        } else {
            let pct = free / total * 100.0;
            let msg = format!("free space {pct:.1}%");
            if pct < t.crit_pct {
                components.push(Component::new("free space", Status::Critical, true, msg));
            } else if pct < t.warn_pct {
                components.push(Component::new("free space", Status::Degraded, false, msg));
            } else {
                components.push(Component::new("free space", Status::Ok, false, msg));
            }
        }

        CheckReport::from_components(components)
    }
}

#[async_trait]
impl CheckType for UnraidCheck {
    fn type_id(&self) -> &'static str {
        "unraid"
    }

    fn schema(&self) -> ConfigSchema {
        ConfigSchema {
            fields: vec![
                Field {
                    name: "url",
                    kind: FieldKind::String,
                    required: true,
                    default: None,
                    help: "Unraid base URL, e.g. http://tower.local",
                    secret: false,
                },
                Field {
                    name: "api_key",
                    kind: FieldKind::String,
                    required: true,
                    default: None,
                    help: "Unraid API key (Settings -> Management Access -> API Keys)",
                    secret: true,
                },
                Field {
                    name: "free_space_warn_pct",
                    kind: FieldKind::Float,
                    required: false,
                    default: Some(json!(10.0)),
                    help: "Array free %% at/below this is Degraded",
                    secret: false,
                },
                Field {
                    name: "free_space_critical_pct",
                    kind: FieldKind::Float,
                    required: false,
                    default: Some(json!(3.0)),
                    help: "Array free %% at/below this is Critical",
                    secret: false,
                },
                Field {
                    name: "disk_temp_warn_c",
                    kind: FieldKind::Int,
                    required: false,
                    default: Some(json!(55)),
                    help: "Disk temp (C) at/above this is Degraded",
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
        let cfg: UnraidConfig = match serde_json::from_value(cfg.clone()) {
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
        let endpoint = format!("{}/graphql", cfg.url.trim_end_matches('/'));
        let resp = match client
            .post(&endpoint)
            .header("x-api-key", &cfg.api_key)
            .json(&json!({ "query": QUERY }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return CheckReport::new(Status::Unknown, format!("request failed: {e}")),
        };
        let parsed: GqlResponse = match resp.json().await {
            Ok(p) => p,
            Err(e) => return CheckReport::new(Status::Unknown, format!("bad response: {e}")),
        };
        let array = match parsed.data {
            Some(d) => d.array,
            None => {
                return CheckReport::new(
                    Status::Unknown,
                    "graphql returned no data (auth or query error)",
                );
            }
        };
        let thresholds = Thresholds {
            warn_pct: cfg.free_space_warn_pct,
            crit_pct: cfg.free_space_critical_pct,
            temp_warn: cfg.disk_temp_warn_c,
        };
        UnraidCheck::evaluate(array, &thresholds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(name: &str, status: &str, temp: Option<i64>, errors: i64) -> Disk {
        Disk {
            name: name.into(),
            status: status.into(),
            temp,
            num_errors: errors,
        }
    }
    fn thresholds() -> Thresholds {
        Thresholds {
            warn_pct: 10.0,
            crit_pct: 3.0,
            temp_warn: 55,
        }
    }
    fn array(
        state: &str,
        free: &str,
        total: &str,
        disks: Vec<Disk>,
        parities: Vec<Disk>,
        caches: Vec<Disk>,
    ) -> ArrayData {
        ArrayData {
            state: state.into(),
            capacity: Capacity {
                kilobytes: Kilobytes {
                    free: free.into(),
                    total: total.into(),
                },
            },
            disks,
            parities,
            caches,
        }
    }

    #[test]
    fn all_healthy_ample_space_is_ok() {
        let a = array(
            "STARTED",
            "500",
            "1000",
            vec![disk("disk1", "DISK_OK", Some(40), 0)],
            vec![disk("parity", "DISK_OK", Some(42), 0)],
            vec![disk("cache", "DISK_OK", Some(37), 0)],
        );
        let r = UnraidCheck::evaluate(a, &thresholds());
        assert_eq!(r.status, Status::Ok);
        assert_eq!(r.components.len(), 5); // array + parity + disk1 + cache + free space
    }

    #[test]
    fn failed_data_disk_is_critical_and_named() {
        let a = array(
            "STARTED",
            "500",
            "1000",
            vec![disk("disk3", "DISK_DSBL", Some(45), 0)],
            vec![disk("parity", "DISK_OK", Some(42), 0)],
            vec![],
        );
        let r = UnraidCheck::evaluate(a, &thresholds());
        assert_eq!(r.status, Status::Critical);
        assert!(r.message.contains("disk3"));
    }

    #[test]
    fn low_free_space_degrades() {
        // 5% free, warn at 10% -> Degraded (non-critical component).
        let a = array(
            "STARTED",
            "50",
            "1000",
            vec![disk("disk1", "DISK_OK", Some(40), 0)],
            vec![],
            vec![],
        );
        let r = UnraidCheck::evaluate(a, &thresholds());
        assert_eq!(r.status, Status::Degraded);
    }

    #[test]
    fn very_low_free_space_is_critical() {
        // 1% free, critical at 3% -> Critical.
        let a = array(
            "STARTED",
            "10",
            "1000",
            vec![disk("disk1", "DISK_OK", Some(40), 0)],
            vec![],
            vec![],
        );
        let r = UnraidCheck::evaluate(a, &thresholds());
        assert_eq!(r.status, Status::Critical);
    }

    #[test]
    fn hot_disk_degrades() {
        let a = array(
            "STARTED",
            "500",
            "1000",
            vec![disk("disk1", "DISK_OK", Some(60), 0)],
            vec![],
            vec![],
        );
        let r = UnraidCheck::evaluate(a, &thresholds());
        assert_eq!(r.status, Status::Degraded);
    }

    #[test]
    fn cache_failure_only_degrades() {
        // cache is non-critical -> a failed cache caps at Degraded.
        let a = array(
            "STARTED",
            "500",
            "1000",
            vec![disk("disk1", "DISK_OK", Some(40), 0)],
            vec![],
            vec![disk("cache", "DISK_DSBL", Some(37), 0)],
        );
        let r = UnraidCheck::evaluate(a, &thresholds());
        assert_eq!(r.status, Status::Degraded);
    }

    #[test]
    fn array_stopped_is_critical() {
        let a = array("STOPPED", "500", "1000", vec![], vec![], vec![]);
        let r = UnraidCheck::evaluate(a, &thresholds());
        assert_eq!(r.status, Status::Critical);
    }

    #[tokio::test]
    async fn run_parses_graphql_and_reports() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let body = json!({ "data": { "array": {
            "state": "STARTED",
            "capacity": { "kilobytes": { "free": "50", "total": "1000" } },
            "disks": [{ "name": "disk1", "status": "DISK_OK", "temp": 40, "numErrors": 0 }],
            "parities": [], "caches": []
        }}});
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let cfg = json!({ "url": server.uri(), "api_key": "k" });
        let report = UnraidCheck.run(&cfg).await;
        // 5% free < 10% warn -> Degraded
        assert_eq!(report.status, Status::Degraded);
    }

    #[tokio::test]
    async fn run_unreachable_is_unknown() {
        let cfg = json!({
            "url": "http://127.0.0.1:1",
            "api_key": "k",
            "timeout_secs": 1
        });
        assert_eq!(UnraidCheck.run(&cfg).await.status, Status::Unknown);
    }

    #[tokio::test]
    async fn run_bad_config_is_unknown() {
        assert_eq!(
            UnraidCheck.run(&json!({ "url": "http://x" })).await.status,
            Status::Unknown
        );
    }
}
