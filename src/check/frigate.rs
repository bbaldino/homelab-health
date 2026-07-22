use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::{CheckReport, Component};
use crate::status::Status;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrigateConfig {
    base_url: String,
    #[serde(default = "default_min_fps")]
    min_camera_fps: f64,
}

fn default_min_fps() -> f64 {
    0.1
}

#[derive(Deserialize)]
struct CameraStats {
    #[serde(default)]
    camera_fps: f64,
    #[serde(default)]
    process_fps: f64,
}

// NOTE: no deny_unknown_fields here — Frigate's /api/stats carries many fields we don't model.
#[derive(Deserialize)]
struct Stats {
    cameras: HashMap<String, CameraStats>,
}

pub struct FrigateCameraCheck;

#[async_trait]
impl CheckType for FrigateCameraCheck {
    fn type_id(&self) -> &'static str {
        "frigate-camera"
    }

    fn schema(&self) -> ConfigSchema {
        ConfigSchema {
            fields: vec![
                Field {
                    name: "base_url",
                    kind: FieldKind::String,
                    required: true,
                    default: None,
                    help: "Frigate base URL, e.g. http://frigate.lan:5000",
                },
                Field {
                    name: "min_camera_fps",
                    kind: FieldKind::Float,
                    required: false,
                    default: Some(json!(0.1)),
                    help: "camera_fps at or below this is treated as a dead feed",
                },
            ],
        }
    }

    async fn run(&self, cfg: &Value) -> CheckReport {
        let cfg: FrigateConfig = match serde_json::from_value(cfg.clone()) {
            Ok(c) => c,
            Err(e) => return CheckReport::new(Status::Unknown, format!("bad config: {e}")),
        };

        let url = format!("{}/api/stats", cfg.base_url.trim_end_matches('/'));
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => return CheckReport::new(Status::Unknown, format!("client error: {e}")),
        };

        let stats: Stats = match client.get(&url).send().await {
            Ok(resp) => match resp.json().await {
                Ok(s) => s,
                Err(e) => return CheckReport::new(Status::Unknown, format!("bad stats json: {e}")),
            },
            Err(e) => return CheckReport::new(Status::Unknown, format!("request failed: {e}")),
        };

        FrigateCameraCheck::evaluate(&stats, cfg.min_camera_fps)
    }
}

impl FrigateCameraCheck {
    fn evaluate(stats: &Stats, min_fps: f64) -> CheckReport {
        if stats.cameras.is_empty() {
            return CheckReport::new(Status::Unknown, "no cameras reported by Frigate");
        }
        let mut components: Vec<Component> = stats
            .cameras
            .iter()
            .map(|(name, cam)| {
                if cam.camera_fps <= min_fps {
                    Component::new(
                        name,
                        Status::Critical,
                        true,
                        format!("camera_fps={:.2} (feed down)", cam.camera_fps),
                    )
                } else if cam.process_fps == 0.0 {
                    Component::new(
                        name,
                        Status::Degraded,
                        false,
                        "process_fps=0 (detection stalled)",
                    )
                } else {
                    Component::new(
                        name,
                        Status::Ok,
                        true,
                        format!("camera_fps={:.1}", cam.camera_fps),
                    )
                }
            })
            .collect();
        components.sort_by(|a, b| a.name.cmp(&b.name));
        CheckReport::from_components(components)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn stats_json() -> Value {
        json!({
            "cameras": {
                "driveway": { "camera_fps": 0.0, "process_fps": 0.0 },
                "backyard": { "camera_fps": 5.0, "process_fps": 5.0 }
            }
        })
    }

    #[test]
    fn dead_camera_is_critical_and_named() {
        let stats: Stats = serde_json::from_value(stats_json()).unwrap();
        let report = FrigateCameraCheck::evaluate(&stats, 0.1);
        assert_eq!(report.status, Status::Critical);
        assert!(report.message.contains("driveway"));
        assert_eq!(report.components.len(), 2);
    }

    #[test]
    fn all_healthy_is_ok() {
        let stats: Stats = serde_json::from_value(json!({
            "cameras": { "a": { "camera_fps": 5.0, "process_fps": 5.0 } }
        }))
        .unwrap();
        let report = FrigateCameraCheck::evaluate(&stats, 0.1);
        assert_eq!(report.status, Status::Ok);
    }

    #[tokio::test]
    async fn fetches_and_reports_over_http() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats"))
            .respond_with(ResponseTemplate::new(200).set_body_json(stats_json()))
            .mount(&server)
            .await;

        let cfg = json!({ "base_url": server.uri() });
        let report = FrigateCameraCheck.run(&cfg).await;
        assert_eq!(report.status, Status::Critical);
    }

    #[tokio::test]
    async fn unreachable_is_unknown() {
        let cfg = json!({ "base_url": "http://127.0.0.1:1" });
        let report = FrigateCameraCheck.run(&cfg).await;
        assert_eq!(report.status, Status::Unknown);
    }

    #[test]
    fn empty_cameras_is_unknown() {
        let stats: Stats = serde_json::from_value(json!({ "cameras": {} })).unwrap();
        let report = FrigateCameraCheck::evaluate(&stats, 0.1);
        assert_eq!(report.status, Status::Unknown);
    }
}
