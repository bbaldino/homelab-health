use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::{CheckReport, Component};
use crate::status::Status;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MusicAssistantConfig {
    url: String,
    token: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_timeout() -> u64 {
    10
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProviderInstance {
    instance_id: String,
    #[serde(default)]
    available: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProviderConfig {
    instance_id: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "type", default)]
    provider_type: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    last_error: Option<String>,
}

pub struct MusicAssistantCheck;

impl MusicAssistantCheck {
    /// Pure mapping: join instances (availability) with configs (enabled +
    /// last_error) by instance_id; one component per enabled music/player provider.
    fn evaluate(instances: Vec<ProviderInstance>, configs: Vec<ProviderConfig>) -> CheckReport {
        let available: HashMap<String, bool> = instances
            .into_iter()
            .map(|i| (i.instance_id, i.available))
            .collect();

        let mut components: Vec<Component> = Vec::new();
        for c in configs {
            if !c.enabled {
                continue;
            }
            if c.provider_type != "music" && c.provider_type != "player" {
                continue;
            }
            let display = c
                .name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| c.domain.clone());
            let critical = c.provider_type == "music";
            let is_available = available.get(&c.instance_id).copied().unwrap_or(false);
            if is_available && c.last_error.is_none() {
                components.push(Component::new(display, Status::Ok, critical, "available"));
            } else {
                let msg = c
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "provider unavailable".to_string());
                components.push(Component::new(display, Status::Critical, critical, msg));
            }
        }

        if components.is_empty() {
            return CheckReport::new(Status::Unknown, "no music or player providers enabled");
        }
        components.sort_by(|a, b| a.name.cmp(&b.name));
        CheckReport::from_components(components)
    }
}

#[async_trait]
impl CheckType for MusicAssistantCheck {
    fn type_id(&self) -> &'static str {
        "music-assistant"
    }

    fn schema(&self) -> ConfigSchema {
        ConfigSchema {
            fields: vec![
                Field {
                    name: "url",
                    kind: FieldKind::String,
                    required: true,
                    default: None,
                    help: "Music Assistant base URL, e.g. http://music-assistant.local:8095",
                },
                Field {
                    name: "token",
                    kind: FieldKind::String,
                    required: true,
                    default: None,
                    help: "Music Assistant long-lived API token",
                },
                Field {
                    name: "timeout_secs",
                    kind: FieldKind::Int,
                    required: false,
                    default: Some(json!(10)),
                    help: "Overall timeout in seconds",
                },
            ],
        }
    }

    async fn run(&self, cfg: &Value) -> CheckReport {
        let cfg: MusicAssistantConfig = match serde_json::from_value(cfg.clone()) {
            Ok(c) => c,
            Err(e) => return CheckReport::new(Status::Unknown, format!("bad config: {e}")),
        };
        let ws_url = to_ws_url(&cfg.url);
        let work = fetch_and_evaluate(ws_url, cfg.token);
        match tokio::time::timeout(Duration::from_secs(cfg.timeout_secs), work).await {
            Ok(Ok(report)) => report,
            Ok(Err(e)) => CheckReport::new(Status::Unknown, e),
            Err(_) => CheckReport::new(Status::Unknown, "music assistant check timed out"),
        }
    }
}

/// Convert an http(s) base URL into the MA websocket URL (…/ws).
fn to_ws_url(url: &str) -> String {
    let base = url.trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{base}/ws")
}

async fn fetch_and_evaluate(ws_url: String, token: String) -> Result<CheckReport, String> {
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;

    // Discard the hello (first server message).
    recv_reply(&mut ws, None).await?;

    // Authenticate.
    send_command(&mut ws, "auth", "auth", Some(json!({ "token": token }))).await?;
    let auth = recv_reply(&mut ws, Some("auth")).await?;
    if auth.get("error_code").is_some() {
        return Err(format!(
            "auth failed: {}",
            auth.get("details").and_then(|d| d.as_str()).unwrap_or("")
        ));
    }

    let instances: Vec<ProviderInstance> = query(&mut ws, "p", "providers").await?;
    let configs: Vec<ProviderConfig> = query(&mut ws, "c", "config/providers").await?;
    Ok(MusicAssistantCheck::evaluate(instances, configs))
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send_command(
    ws: &mut Ws,
    message_id: &str,
    command: &str,
    args: Option<Value>,
) -> Result<(), String> {
    let mut msg = json!({ "message_id": message_id, "command": command });
    if let Some(a) = args {
        msg["args"] = a;
    }
    ws.send(Message::text(msg.to_string()))
        .await
        .map_err(|e| format!("send failed: {e}"))
}

/// Read text frames until one matches `want_id` (or the first frame if None).
async fn recv_reply(ws: &mut Ws, want_id: Option<&str>) -> Result<Value, String> {
    while let Some(item) = ws.next().await {
        let msg = item.map_err(|e| format!("ws error: {e}"))?;
        if msg.is_close() {
            return Err("connection closed before reply".to_string());
        }
        if !msg.is_text() {
            continue;
        }
        let txt = msg.to_text().map_err(|e| format!("utf8 error: {e}"))?;
        let v: Value = serde_json::from_str(txt).map_err(|e| format!("bad json: {e}"))?;
        match want_id {
            None => return Ok(v),
            Some(id) => {
                if v.get("message_id").and_then(|m| m.as_str()) == Some(id) {
                    return Ok(v);
                }
            }
        }
    }
    Err("connection ended before reply".to_string())
}

async fn query<T: for<'de> Deserialize<'de>>(
    ws: &mut Ws,
    message_id: &str,
    command: &str,
) -> Result<Vec<T>, String> {
    send_command(ws, message_id, command, None).await?;
    let reply = recv_reply(ws, Some(message_id)).await?;
    if let Some(code) = reply.get("error_code") {
        return Err(format!("{command} error {code}"));
    }
    let result = reply
        .get("result")
        .cloned()
        .ok_or_else(|| format!("{command}: no result"))?;
    serde_json::from_value(result).map_err(|e| format!("{command}: bad result: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(id: &str, available: bool) -> ProviderInstance {
        ProviderInstance {
            instance_id: id.into(),
            available,
        }
    }
    fn cfg(
        id: &str,
        ptype: &str,
        name: &str,
        enabled: bool,
        last_error: Option<&str>,
    ) -> ProviderConfig {
        ProviderConfig {
            instance_id: id.into(),
            domain: id.into(),
            name: Some(name.into()),
            provider_type: ptype.into(),
            enabled,
            last_error: last_error.map(|s| s.into()),
        }
    }

    #[test]
    fn all_healthy_is_ok() {
        let report = MusicAssistantCheck::evaluate(
            vec![inst("spotify", true), inst("sonos", true)],
            vec![
                cfg("spotify", "music", "Spotify", true, None),
                cfg("sonos", "player", "SONOS", true, None),
            ],
        );
        assert_eq!(report.status, Status::Ok);
        assert_eq!(report.components.len(), 2);
    }

    #[test]
    fn unavailable_music_provider_is_critical_and_named() {
        let report = MusicAssistantCheck::evaluate(
            vec![inst("spotify", false), inst("sonos", true)],
            vec![
                cfg("spotify", "music", "Spotify", true, None),
                cfg("sonos", "player", "SONOS", true, None),
            ],
        );
        assert_eq!(report.status, Status::Critical);
        assert!(report.message.contains("Spotify"));
    }

    #[test]
    fn player_provider_down_caps_at_degraded() {
        let report = MusicAssistantCheck::evaluate(
            vec![inst("spotify", true), inst("sonos", false)],
            vec![
                cfg("spotify", "music", "Spotify", true, None),
                cfg("sonos", "player", "SONOS", true, None),
            ],
        );
        assert_eq!(report.status, Status::Degraded);
    }

    #[test]
    fn last_error_surfaces_as_message() {
        let report = MusicAssistantCheck::evaluate(
            vec![inst("spotify", true)],
            vec![cfg(
                "spotify",
                "music",
                "Spotify",
                true,
                Some("token refresh failed"),
            )],
        );
        assert_eq!(report.status, Status::Critical);
        assert!(report.components[0].message.contains("token refresh"));
    }

    #[test]
    fn metadata_and_disabled_providers_are_ignored() {
        let report = MusicAssistantCheck::evaluate(
            vec![inst("spotify", true)],
            vec![
                cfg("spotify", "music", "Spotify", true, None),
                cfg("musicbrainz", "metadata", "MusicBrainz", true, None), // wrong type
                cfg("tidal", "music", "Tidal", false, None),               // disabled
            ],
        );
        assert_eq!(report.components.len(), 1);
        assert_eq!(report.status, Status::Ok);
    }

    #[test]
    fn no_relevant_providers_is_unknown() {
        let report = MusicAssistantCheck::evaluate(
            vec![],
            vec![cfg("musicbrainz", "metadata", "MusicBrainz", true, None)],
        );
        assert_eq!(report.status, Status::Unknown);
    }

    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    // Minimal fake MA WS server: hello, then answer auth/providers/config-providers.
    async fn fake_ma(providers: Value, configs: Value) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                // hello
                let _ = ws
                    .send(Message::text(json!({"server_version":"2.9.9"}).to_string()))
                    .await;
                while let Some(Ok(msg)) = ws.next().await {
                    if !msg.is_text() {
                        continue;
                    }
                    let v: Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
                    let mid = v["message_id"].as_str().unwrap_or("").to_string();
                    let reply = match v["command"].as_str().unwrap_or("") {
                        "auth" => json!({ "message_id": mid, "result": null }),
                        "providers" => json!({ "message_id": mid, "result": providers }),
                        "config/providers" => json!({ "message_id": mid, "result": configs }),
                        _ => json!({ "message_id": mid, "error_code": 1, "details": "unknown" }),
                    };
                    let _ = ws.send(Message::text(reply.to_string())).await;
                }
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn run_reports_ok_over_websocket() {
        let url = fake_ma(
            json!([{ "instance_id": "spotify", "available": true }]),
            json!([{ "instance_id": "spotify", "domain": "spotify", "name": "Spotify",
                     "type": "music", "enabled": true, "last_error": null }]),
        )
        .await;
        let cfg = json!({ "url": url, "token": "t" });
        let report = MusicAssistantCheck.run(&cfg).await;
        assert_eq!(report.status, Status::Ok);
    }

    #[tokio::test]
    async fn run_reports_critical_when_spotify_unavailable() {
        let url = fake_ma(
            json!([{ "instance_id": "spotify", "available": false }]),
            json!([{ "instance_id": "spotify", "domain": "spotify", "name": "Spotify",
                     "type": "music", "enabled": true, "last_error": null }]),
        )
        .await;
        let report = MusicAssistantCheck
            .run(&json!({ "url": url, "token": "t" }))
            .await;
        assert_eq!(report.status, Status::Critical);
        assert!(report.message.contains("Spotify"));
    }

    #[tokio::test]
    async fn run_unreachable_is_unknown() {
        let report = MusicAssistantCheck
            .run(&json!({ "url": "http://127.0.0.1:1", "token": "t", "timeout_secs": 1 }))
            .await;
        assert_eq!(report.status, Status::Unknown);
    }

    #[tokio::test]
    async fn run_bad_config_is_unknown() {
        let report = MusicAssistantCheck.run(&json!({ "url": "http://x" })).await; // missing token
        assert_eq!(report.status, Status::Unknown);
    }
}
