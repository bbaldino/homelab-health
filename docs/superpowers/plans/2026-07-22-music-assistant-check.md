# music-assistant Check Implementation Plan

> **For agentic workers:** implement task-by-task with TDD. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Add a `music-assistant` check type that connects to Music Assistant's authenticated WebSocket API, reads provider health, and maps each enabled music/player provider onto the status/component model (so "Spotify logged out" → Critical, "a player provider down" → Degraded). Register it as a builtin.

**Architecture:** New plugin `src/check/music_assistant.rs`. A pure `evaluate(instances, configs) -> CheckReport` helper (hermetic-testable) holds the mapping/rollup logic; an async `run` connects over WebSocket (tokio-tungstenite), authenticates with a token, queries `providers` + `config/providers`, and feeds the parsed data to `evaluate`. First check that isn't plain HTTP.

**Tech Stack:** Rust, tokio-tungstenite + futures-util (new), serde, async-trait. Existing check/report/status modules.

**MA protocol (verified against MA 2.9.9):**
- WS endpoint `ws://<host>:<port>/ws`. On connect the server sends a hello (ServerInfoMessage) — read and discard.
- Auth: send `{"message_id":"auth","command":"auth","args":{"token":"<token>"}}`; reply carries `message_id:"auth"` and, on success, no `error_code`.
- `{"message_id":"1","command":"providers"}` → reply `{"message_id":"1","result":[ProviderInstance…]}`. ProviderInstance has `instance_id`, `available: bool` (+ others).
- `{"message_id":"2","command":"config/providers"}` → reply `result:[ProviderConfig…]`. ProviderConfig has `instance_id`, `domain`, `name` (nullable), `type`, `enabled: bool`, `last_error` (nullable string).
- Replies are correlated by `message_id`; event/broadcast messages have no matching `message_id` and must be skipped.

## Global Constraints

- Only capitalize the first letter of multi-letter acronyms — `MusicAssistantCheck`, `MusicAssistantConfig`.
- Add deps with `cargo add` (this plan adds `tokio-tungstenite` and `futures-util`).
- Format with `cargo +nightly fmt` before every commit.
- A check must never panic; connect/auth/protocol/timeout/parse failures return `Status::Unknown`.
- Component `message` must be non-empty when status != Ok (Component::new debug-asserts).

---

### Task 1: Provider types + pure evaluate mapping

**Files:**
- Create: `src/check/music_assistant.rs`
- Modify: `src/check/mod.rs` (add `pub mod music_assistant;`)

**Interfaces:**
- Produces: `struct MusicAssistantCheck`; `ProviderInstance`/`ProviderConfig` (Deserialize); `MusicAssistantCheck::evaluate(instances: Vec<ProviderInstance>, configs: Vec<ProviderConfig>) -> CheckReport`.
- Mapping: only `enabled` providers whose `type` is `music` or `player` become components; `available && last_error.is_none()` → Ok else Critical; `music` → critical=true, `player` → critical=false; empty → `Unknown`.

- [ ] **Step 1: Write the failing tests**

Create `src/check/music_assistant.rs`:
```rust
use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::{CheckReport, Component};
use crate::status::Status;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(id: &str, available: bool) -> ProviderInstance {
        ProviderInstance {
            instance_id: id.into(),
            available,
        }
    }
    fn cfg(id: &str, ptype: &str, name: &str, enabled: bool, last_error: Option<&str>) -> ProviderConfig {
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
            vec![cfg("spotify", "music", "Spotify", true, Some("token refresh failed"))],
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
}
```

Add to `src/check/mod.rs`:
```rust
pub mod music_assistant;
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test check::music_assistant`
Expected: FAIL until the module compiles/evaluate exists (the tests reference private items in the same module, so this is a compile-then-pass task).

- [ ] **Step 3: Confirm the code above compiles and passes**

The `evaluate` impl is included in Step 1. Run: `cargo test check::music_assistant`
Expected: PASS (6 tests).

- [ ] **Step 4: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: music-assistant provider mapping (pure evaluate)"
```

---

### Task 2: WebSocket run + register builtin

**Files:**
- Modify: `src/check/music_assistant.rs` (add deps usage, `CheckType` impl with WS `run`, integration test)
- Modify: `src/check/mod.rs` (register in `with_builtins`; bump the with_builtins test to expect 5 and assert `get("music-assistant")`)
- Modify: `src/api.rs` (bump the `check_types_lists_builtins` count 4 → 5)

**Interfaces:**
- Produces: `MusicAssistantCheck` implements `CheckType`, `type_id() == "music-assistant"`. Config JSON: `{ "url": String, "token": String, "timeout_secs": u64 (default 10) }`.

- [ ] **Step 1: Add dependencies**

Run:
```bash
cargo add tokio-tungstenite
cargo add futures-util
```

- [ ] **Step 2: Write the failing integration test**

Add to the `#[cfg(test)] mod tests` block in `src/check/music_assistant.rs`:
```rust
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
        let report = MusicAssistantCheck.run(&json!({ "url": url, "token": "t" })).await;
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test check::music_assistant`
Expected: FAIL — `MusicAssistantCheck` does not implement `run`.

- [ ] **Step 4: Implement the WebSocket run**

Add these imports at the top of `src/check/music_assistant.rs` (next to the existing `use` lines):
```rust
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
```

Add the CheckType impl + helpers (above the `tests` module):
```rust
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

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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
```

Note: the exact `tokio-tungstenite` `Message` text API can vary by version. If `Message::text(...)` / `msg.to_text()` / `msg.is_text()` don't match the installed version, adapt them (e.g. `Message::Text(s.into())`, `msg.into_text()`) — verify with `cargo build`. Do not change the protocol logic.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test check::music_assistant`
Expected: PASS (6 evaluate + 4 run tests). Adjust the `Message` API per the note above if it doesn't compile, then re-run.

- [ ] **Step 6: Register as a builtin**

In `src/check/mod.rs`, in `Registry::with_builtins`, add:
```rust
        reg.register(Arc::new(crate::check::music_assistant::MusicAssistantCheck));
```
Update the `with_builtins` test: bump `schemas().len()` from `4` to `5` and add `assert!(reg.get("music-assistant").is_some());`.

In `src/api.rs`, the `check_types_lists_builtins` test asserts `arr.len() == 4` — bump it to `5`.

- [ ] **Step 7: Full suite + commit**

Run: `cargo test`
Expected: PASS (all prior + new).

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: music-assistant websocket check and register as builtin"
```

---

## What this delivers

A `music-assistant` check that logs into MA's authenticated WS API and reports per-provider health: Spotify (and other music sources) as critical components, players as non-critical, with `last_error` surfaced in messages. Config is `{ url, token }`. Registered as a builtin so the running daemon exposes and runs it.

## Notes / future

- Token is stored plaintext in the monitor config (LAN homelab; acceptable). If secrets-at-rest matters later, add encrypted config alongside the Plan 2b notifier settings work.
- Criticality is currently type-based (`music` critical, `player` not). A future config knob could let the user mark specific provider domains critical/non-critical.
- `wss://` support depends on the tokio-tungstenite TLS feature; LAN MA is plain `ws://`. Add a TLS feature if a remote MA over TLS is ever needed.
