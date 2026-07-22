# Health Monitor — Core Engine & Checks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the tested backend foundation of the homelab health monitor: the status/rollup model, the pluggable check-type registry, three real check types (`http`, `tcp`, `frigate-camera`), and SQLite persistence for monitors and their latest status.

**Architecture:** A single Rust crate. A `Status` enum and rollup function express per-component severity with critical/non-critical semantics. Check types implement one `CheckType` trait (id + config schema + async `run`) and register into a `Registry` keyed by type id; monitor *instances* are just data. A thin `Store` persists monitor config and current status in SQLite. This plan produces a library with green tests — no HTTP server or scheduler yet (those are Plan 2).

**Tech Stack:** Rust, tokio, serde/serde_json, async-trait, thiserror, reqwest (rustls), sqlx (sqlite). Dev: wiremock, tempfile.

## Global Constraints

- Only capitalize the first letter of multi-letter acronyms (e.g. `HttpCheck`, not `HTTPCheck`).
- Add dependencies with `cargo add` (never hand-edit versions) so we get the latest.
- Format with `cargo +nightly fmt` before every commit.
- All check config is passed as `serde_json::Value`; each check deserializes its own typed config and returns `Status::Unknown` (with a message) on config or runtime failure — a check must never panic.
- `message` is REQUIRED (non-empty) on any `CheckReport` or `Component` whose status is not `Ok`.

---

### Task 1: Project scaffold + Status enum

**Files:**
- Create: `Cargo.toml` (via `cargo init`)
- Create: `src/lib.rs`
- Create: `src/status.rs`

**Interfaces:**
- Produces: `enum Status { Ok, Degraded, Critical, Unknown }` (serde snake_case), `Status::rank(&self) -> u8` where `Ok=0, Degraded=1, Unknown=2, Critical=3`.

- [ ] **Step 1: Scaffold the crate**

Run:
```bash
cargo init --lib --name homelab-health
cargo add serde --features derive
cargo add serde_json
```

- [ ] **Step 2: Write the failing test**

Create `src/status.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Degraded,
    Critical,
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_orders_ok_lowest_critical_highest() {
        assert!(Status::Ok.rank() < Status::Degraded.rank());
        assert!(Status::Degraded.rank() < Status::Unknown.rank());
        assert!(Status::Unknown.rank() < Status::Critical.rank());
    }

    #[test]
    fn serializes_snake_case() {
        assert_eq!(serde_json::to_string(&Status::Ok).unwrap(), "\"ok\"");
        assert_eq!(
            serde_json::to_string(&Status::Critical).unwrap(),
            "\"critical\""
        );
    }
}
```

Add to `src/lib.rs`:
```rust
pub mod status;
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test status::`
Expected: FAIL — `no method named rank found for enum Status`.

- [ ] **Step 4: Implement `rank`**

Add to `src/status.rs` (above the `tests` module):
```rust
impl Status {
    /// Severity ordering used to pick the "worst" status during rollup.
    pub fn rank(&self) -> u8 {
        match self {
            Status::Ok => 0,
            Status::Degraded => 1,
            Status::Unknown => 2,
            Status::Critical => 3,
        }
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test status::`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add Status enum with severity rank"
```

---

### Task 2: CheckReport + rollup

**Files:**
- Create: `src/report.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `Status`, `Status::rank` (Task 1).
- Produces:
  - `struct Component { name: String, status: Status, critical: bool, message: String }`
  - `struct CheckReport { status: Status, message: String, components: Vec<Component> }`
  - `CheckReport::ok(message: impl Into<String>) -> CheckReport`
  - `CheckReport::new(status: Status, message: impl Into<String>) -> CheckReport`
  - `CheckReport::from_components(components: Vec<Component>) -> CheckReport` (applies rollup)
  - `fn rollup(components: &[Component]) -> (Status, String)`

- [ ] **Step 1: Write the failing tests**

Create `src/report.rs`:
```rust
use crate::status::Status;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Component {
    pub name: String,
    pub status: Status,
    pub critical: bool,
    #[serde(default)]
    pub message: String,
}

impl Component {
    pub fn new(
        name: impl Into<String>,
        status: Status,
        critical: bool,
        message: impl Into<String>,
    ) -> Self {
        Component {
            name: name.into(),
            status,
            critical,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckReport {
    pub status: Status,
    pub message: String,
    #[serde(default)]
    pub components: Vec<Component>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, status: Status, critical: bool) -> Component {
        Component::new(name, status, critical, "detail")
    }

    #[test]
    fn all_ok_rolls_up_to_ok() {
        let (status, _) = rollup(&[c("a", Status::Ok, true), c("b", Status::Ok, false)]);
        assert_eq!(status, Status::Ok);
    }

    #[test]
    fn critical_component_makes_parent_critical() {
        let (status, msg) = rollup(&[c("a", Status::Ok, true), c("driveway", Status::Critical, true)]);
        assert_eq!(status, Status::Critical);
        assert!(msg.contains("driveway"));
    }

    #[test]
    fn noncritical_critical_component_caps_at_degraded() {
        let (status, _) = rollup(&[c("detector", Status::Critical, false)]);
        assert_eq!(status, Status::Degraded);
    }

    #[test]
    fn noncritical_unknown_degrades() {
        let (status, _) = rollup(&[c("x", Status::Unknown, false)]);
        assert_eq!(status, Status::Degraded);
    }

    #[test]
    fn critical_unknown_surfaces_as_unknown() {
        let (status, _) = rollup(&[c("x", Status::Unknown, true)]);
        assert_eq!(status, Status::Unknown);
    }

    #[test]
    fn critical_beats_unknown() {
        let (status, _) = rollup(&[
            c("a", Status::Unknown, true),
            c("b", Status::Critical, true),
        ]);
        assert_eq!(status, Status::Critical);
    }

    #[test]
    fn from_components_sets_rolled_up_status() {
        let report = CheckReport::from_components(vec![c("cam", Status::Critical, true)]);
        assert_eq!(report.status, Status::Critical);
        assert_eq!(report.components.len(), 1);
    }
}
```

Add to `src/lib.rs`:
```rust
pub mod report;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test report::`
Expected: FAIL — `cannot find function rollup`, `no function from_components`.

- [ ] **Step 3: Implement rollup + constructors**

Add to `src/report.rs` (above the `tests` module):
```rust
impl CheckReport {
    pub fn new(status: Status, message: impl Into<String>) -> Self {
        CheckReport {
            status,
            message: message.into(),
            components: Vec::new(),
        }
    }

    pub fn ok(message: impl Into<String>) -> Self {
        CheckReport::new(Status::Ok, message)
    }

    pub fn from_components(components: Vec<Component>) -> Self {
        let (status, message) = rollup(&components);
        CheckReport {
            status,
            message,
            components,
        }
    }
}

/// Reduce a component's raw status to its effective contribution given
/// its criticality (non-critical failures are capped at Degraded).
fn effective(status: Status, critical: bool) -> Status {
    match (status, critical) {
        (Status::Ok, _) => Status::Ok,
        (Status::Degraded, _) => Status::Degraded,
        (Status::Critical, true) => Status::Critical,
        (Status::Critical, false) => Status::Degraded,
        (Status::Unknown, true) => Status::Unknown,
        (Status::Unknown, false) => Status::Degraded,
    }
}

/// Roll up components into a single (status, message). The parent status is
/// the worst effective status; the message names the driving components.
pub fn rollup(components: &[Component]) -> (Status, String) {
    let mut worst = Status::Ok;
    for comp in components {
        let eff = effective(comp.status, comp.critical);
        if eff.rank() > worst.rank() {
            worst = eff;
        }
    }

    if worst == Status::Ok {
        return (Status::Ok, "all components ok".to_string());
    }

    let drivers: Vec<String> = components
        .iter()
        .filter(|c| effective(c.status, c.critical) == worst)
        .map(|c| c.name.clone())
        .collect();

    (worst, format!("{:?}: {}", worst, drivers.join(", ")))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test report::`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add CheckReport and component rollup"
```

---

### Task 3: CheckType trait, config schema, and registry

**Files:**
- Create: `src/check/mod.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `CheckReport`, `Status` (Tasks 1-2).
- Produces:
  - `enum FieldKind { String, Int, Float, Bool }`
  - `struct Field { name: &'static str, kind: FieldKind, required: bool, default: Option<serde_json::Value>, help: &'static str }`
  - `struct ConfigSchema { fields: Vec<Field> }`
  - `#[async_trait] trait CheckType: Send + Sync { fn type_id(&self) -> &'static str; fn schema(&self) -> ConfigSchema; async fn run(&self, cfg: &serde_json::Value) -> CheckReport; }`
  - `struct Registry` with `new()`, `register(Arc<dyn CheckType>)`, `get(&self, type_id: &str) -> Option<Arc<dyn CheckType>>`, `run(&self, type_id: &str, cfg: &Value) -> CheckReport`, `schemas(&self) -> Vec<(&'static str, ConfigSchema)>`.

- [ ] **Step 1: Add dependencies**

Run:
```bash
cargo add async-trait
cargo add thiserror
```

- [ ] **Step 2: Write the failing test**

Create `src/check/mod.rs`:
```rust
use crate::report::CheckReport;
use crate::status::Status;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    String,
    Int,
    Float,
    Bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Field {
    pub name: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<Value>,
    pub help: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigSchema {
    pub fields: Vec<Field>,
}

#[async_trait]
pub trait CheckType: Send + Sync {
    fn type_id(&self) -> &'static str;
    fn schema(&self) -> ConfigSchema;
    async fn run(&self, cfg: &Value) -> CheckReport;
}

#[derive(Default)]
pub struct Registry {
    types: HashMap<&'static str, Arc<dyn CheckType>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysOk;

    #[async_trait]
    impl CheckType for AlwaysOk {
        fn type_id(&self) -> &'static str {
            "always-ok"
        }
        fn schema(&self) -> ConfigSchema {
            ConfigSchema { fields: vec![] }
        }
        async fn run(&self, _cfg: &Value) -> CheckReport {
            CheckReport::ok("fine")
        }
    }

    #[tokio::test]
    async fn registered_type_runs_via_registry() {
        let mut reg = Registry::new();
        reg.register(Arc::new(AlwaysOk));
        let report = reg.run("always-ok", &Value::Null).await;
        assert_eq!(report.status, Status::Ok);
    }

    #[tokio::test]
    async fn unknown_type_returns_unknown() {
        let reg = Registry::new();
        let report = reg.run("nope", &Value::Null).await;
        assert_eq!(report.status, Status::Unknown);
        assert!(report.message.contains("nope"));
    }
}
```

Add to `src/lib.rs`:
```rust
pub mod check;
```

Run `cargo add tokio --features rt-multi-thread,macros` and add dev usage — the `#[tokio::test]` macro needs tokio. Also run:
```bash
cargo add tokio --features macros,rt-multi-thread
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test check::`
Expected: FAIL — `no function or associated item named new found for struct Registry`.

- [ ] **Step 4: Implement the registry**

Add to `src/check/mod.rs` (above the `tests` module):
```rust
impl Registry {
    pub fn new() -> Self {
        Registry {
            types: HashMap::new(),
        }
    }

    pub fn register(&mut self, check: Arc<dyn CheckType>) {
        self.types.insert(check.type_id(), check);
    }

    pub fn get(&self, type_id: &str) -> Option<Arc<dyn CheckType>> {
        self.types.get(type_id).cloned()
    }

    pub async fn run(&self, type_id: &str, cfg: &Value) -> CheckReport {
        match self.get(type_id) {
            Some(check) => check.run(cfg).await,
            None => CheckReport::new(
                Status::Unknown,
                format!("no check type registered for '{type_id}'"),
            ),
        }
    }

    pub fn schemas(&self) -> Vec<(&'static str, ConfigSchema)> {
        self.types
            .values()
            .map(|c| (c.type_id(), c.schema()))
            .collect()
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test check::`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add CheckType trait, config schema, and registry"
```

---

### Task 4: HTTP check

**Files:**
- Create: `src/check/http.rs`
- Modify: `src/check/mod.rs` (add `pub mod http;`)

**Interfaces:**
- Consumes: `CheckType`, `ConfigSchema`, `Field`, `FieldKind`, `CheckReport`, `Status`.
- Produces: `struct HttpCheck` implementing `CheckType` with `type_id() == "http"`. Config JSON: `{ "url": String (required), "expected_status": u16 (default 200), "timeout_secs": u64 (default 10) }`.

- [ ] **Step 1: Add dependencies**

Run:
```bash
cargo add reqwest --no-default-features --features rustls-tls,json
cargo add --dev wiremock
```

- [ ] **Step 2: Write the failing tests**

Create `src/check/http.rs`:
```rust
use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::CheckReport;
use crate::status::Status;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Deserialize)]
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
}
```

Add to `src/check/mod.rs`:
```rust
pub mod http;
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test check::http`
Expected: FAIL — `HttpCheck` does not implement `run`.

- [ ] **Step 4: Implement HttpCheck**

Add to `src/check/http.rs` (above the `tests` module):
```rust
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
                },
                Field {
                    name: "expected_status",
                    kind: FieldKind::Int,
                    required: false,
                    default: Some(json!(200)),
                    help: "HTTP status code that means healthy",
                },
                Field {
                    name: "timeout_secs",
                    kind: FieldKind::Int,
                    required: false,
                    default: Some(json!(10)),
                    help: "Request timeout in seconds",
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
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test check::http`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add http check type"
```

---

### Task 5: TCP check

**Files:**
- Create: `src/check/tcp.rs`
- Modify: `src/check/mod.rs` (add `pub mod tcp;`)

**Interfaces:**
- Consumes: `CheckType`, schema types, `CheckReport`, `Status`.
- Produces: `struct TcpCheck` implementing `CheckType`, `type_id() == "tcp"`. Config JSON: `{ "host": String (required), "port": u16 (required), "timeout_secs": u64 (default 5) }`.

- [ ] **Step 1: Ensure tokio net is available**

Run:
```bash
cargo add tokio --features net,time
```

- [ ] **Step 2: Write the failing tests**

Create `src/check/tcp.rs`:
```rust
use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::CheckReport;
use crate::status::Status;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
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
```

Add to `src/check/mod.rs`:
```rust
pub mod tcp;
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test check::tcp`
Expected: FAIL — `TcpCheck` does not implement `run`.

- [ ] **Step 4: Implement TcpCheck**

Add to `src/check/tcp.rs` (above the `tests` module):
```rust
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
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test check::tcp`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add tcp check type"
```

---

### Task 6: Frigate camera check (the custom-plugin proof)

**Files:**
- Create: `src/check/frigate.rs`
- Modify: `src/check/mod.rs` (add `pub mod frigate;`)

**Interfaces:**
- Consumes: `CheckType`, schema types, `CheckReport`, `Component`, `CheckReport::from_components`, `Status`.
- Produces: `struct FrigateCameraCheck` implementing `CheckType`, `type_id() == "frigate-camera"`. Config JSON: `{ "base_url": String (required), "min_camera_fps": f64 (default 0.1) }`. Fetches `{base_url}/api/stats`; emits one component per camera (critical if `camera_fps <= min_camera_fps`; degraded if `process_fps == 0`).

- [ ] **Step 1: Write the failing tests**

Create `src/check/frigate.rs`:
```rust
use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::{CheckReport, Component};
use crate::status::Status;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Deserialize)]
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

#[derive(Deserialize)]
struct Stats {
    cameras: HashMap<String, CameraStats>,
}

pub struct FrigateCameraCheck;

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
}
```

Add to `src/check/mod.rs`:
```rust
pub mod frigate;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test check::frigate`
Expected: FAIL — `FrigateCameraCheck` does not implement `run` (the `evaluate` tests may compile but `run` is missing).

- [ ] **Step 3: Implement the CheckType**

Add to `src/check/frigate.rs` (above the `tests` module):
```rust
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
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client builds");

        let stats: Stats = match client.get(&url).send().await {
            Ok(resp) => match resp.json().await {
                Ok(s) => s,
                Err(e) => {
                    return CheckReport::new(Status::Unknown, format!("bad stats json: {e}"))
                }
            },
            Err(e) => return CheckReport::new(Status::Unknown, format!("request failed: {e}")),
        };

        FrigateCameraCheck::evaluate(&stats, cfg.min_camera_fps)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test check::frigate`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add frigate-camera check with per-camera components"
```

---

### Task 7: SQLite store for monitors and current status

**Files:**
- Create: `src/store.rs`
- Create: `migrations/0001_init.sql`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `CheckReport`, `Status`.
- Produces:
  - `struct NewMonitor { name: String, type_id: String, config: Value, interval_secs: i64, enabled: bool }`
  - `struct Monitor { id: i64, name: String, type_id: String, config: Value, interval_secs: i64, enabled: bool }`
  - `struct Store` with:
    - `async fn connect(url: &str) -> Result<Store, sqlx::Error>` (runs migrations)
    - `async fn create_monitor(&self, m: NewMonitor) -> Result<Monitor, sqlx::Error>`
    - `async fn list_monitors(&self) -> Result<Vec<Monitor>, sqlx::Error>`
    - `async fn get_monitor(&self, id: i64) -> Result<Option<Monitor>, sqlx::Error>`
    - `async fn save_status(&self, monitor_id: i64, report: &CheckReport) -> Result<(), sqlx::Error>`
    - `async fn get_current(&self, monitor_id: i64) -> Result<Option<(Status, String)>, sqlx::Error>`

- [ ] **Step 1: Add dependencies**

Run:
```bash
cargo add sqlx --no-default-features --features runtime-tokio-rustls,sqlite,macros
cargo add --dev tempfile
```

- [ ] **Step 2: Write the migration**

Create `migrations/0001_init.sql`:
```sql
CREATE TABLE monitors (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL,
    type_id       TEXT    NOT NULL,
    config_json   TEXT    NOT NULL,
    interval_secs INTEGER NOT NULL,
    enabled       INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE status_current (
    monitor_id      INTEGER PRIMARY KEY REFERENCES monitors(id) ON DELETE CASCADE,
    status          TEXT    NOT NULL,
    message         TEXT    NOT NULL,
    components_json TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL DEFAULT (datetime('now'))
);
```

- [ ] **Step 3: Write the failing tests**

Create `src/store.rs`:
```rust
use crate::report::CheckReport;
use crate::status::Status;
use serde_json::Value;
use sqlx::sqlite::{SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};

pub struct NewMonitor {
    pub name: String,
    pub type_id: String,
    pub config: Value,
    pub interval_secs: i64,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub type_id: String,
    pub config: Value,
    pub interval_secs: i64,
    pub enabled: bool,
}

pub struct Store {
    pool: SqlitePool,
}

fn row_to_monitor(row: SqliteRow) -> Monitor {
    let config_str: String = row.get("config_json");
    Monitor {
        id: row.get("id"),
        name: row.get("name"),
        type_id: row.get("type_id"),
        config: serde_json::from_str(&config_str).unwrap_or(Value::Null),
        interval_secs: row.get("interval_secs"),
        enabled: row.get::<i64, _>("enabled") != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> Store {
        // In-memory DB, single connection so it persists for the test.
        Store::connect("sqlite::memory:").await.unwrap()
    }

    fn sample() -> NewMonitor {
        NewMonitor {
            name: "Plex".into(),
            type_id: "http".into(),
            config: serde_json::json!({ "url": "http://plex.lan" }),
            interval_secs: 30,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn create_then_get_roundtrips() {
        let s = store().await;
        let created = s.create_monitor(sample()).await.unwrap();
        assert!(created.id > 0);
        let fetched = s.get_monitor(created.id).await.unwrap().unwrap();
        assert_eq!(fetched.name, "Plex");
        assert_eq!(fetched.type_id, "http");
    }

    #[tokio::test]
    async fn list_returns_created() {
        let s = store().await;
        s.create_monitor(sample()).await.unwrap();
        let all = s.list_monitors().await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn save_and_get_current_status() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        s.save_status(m.id, &CheckReport::new(Status::Critical, "HTTP 503"))
            .await
            .unwrap();
        let (status, msg) = s.get_current(m.id).await.unwrap().unwrap();
        assert_eq!(status, Status::Critical);
        assert_eq!(msg, "HTTP 503");
    }

    #[tokio::test]
    async fn save_status_upserts() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        s.save_status(m.id, &CheckReport::ok("up")).await.unwrap();
        s.save_status(m.id, &CheckReport::new(Status::Degraded, "slow"))
            .await
            .unwrap();
        let (status, _) = s.get_current(m.id).await.unwrap().unwrap();
        assert_eq!(status, Status::Degraded);
    }
}
```

Add to `src/lib.rs`:
```rust
pub mod store;
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test store::`
Expected: FAIL — `no function connect found for struct Store`.

- [ ] **Step 5: Implement the store**

Add to `src/store.rs` (above the `tests` module):
```rust
impl Store {
    pub async fn connect(url: &str) -> Result<Store, sqlx::Error> {
        // max_connections(1) keeps `sqlite::memory:` alive for the whole test.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(url)
            .await?;
        // raw_sql (not query) so BOTH CREATE TABLE statements run — a prepared
        // query only executes the first statement.
        sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
            .execute(&pool)
            .await?;
        Ok(Store { pool })
    }

    pub async fn create_monitor(&self, m: NewMonitor) -> Result<Monitor, sqlx::Error> {
        let config_str = m.config.to_string();
        let id: i64 = sqlx::query(
            "INSERT INTO monitors (name, type_id, config_json, interval_secs, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id",
        )
        .bind(&m.name)
        .bind(&m.type_id)
        .bind(&config_str)
        .bind(m.interval_secs)
        .bind(m.enabled as i64)
        .fetch_one(&self.pool)
        .await?
        .get("id");

        Ok(Monitor {
            id,
            name: m.name,
            type_id: m.type_id,
            config: m.config,
            interval_secs: m.interval_secs,
            enabled: m.enabled,
        })
    }

    pub async fn list_monitors(&self) -> Result<Vec<Monitor>, sqlx::Error> {
        let rows = sqlx::query("SELECT * FROM monitors ORDER BY id")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(row_to_monitor).collect())
    }

    pub async fn get_monitor(&self, id: i64) -> Result<Option<Monitor>, sqlx::Error> {
        let row = sqlx::query("SELECT * FROM monitors WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(row_to_monitor))
    }

    pub async fn save_status(
        &self,
        monitor_id: i64,
        report: &CheckReport,
    ) -> Result<(), sqlx::Error> {
        let status = serde_json::to_string(&report.status).unwrap();
        let components = serde_json::to_string(&report.components).unwrap();
        sqlx::query(
            "INSERT INTO status_current (monitor_id, status, message, components_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(monitor_id) DO UPDATE SET
                status = excluded.status,
                message = excluded.message,
                components_json = excluded.components_json,
                updated_at = excluded.updated_at",
        )
        .bind(monitor_id)
        .bind(status.trim_matches('"').to_string())
        .bind(&report.message)
        .bind(components)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_current(
        &self,
        monitor_id: i64,
    ) -> Result<Option<(Status, String)>, sqlx::Error> {
        let row = sqlx::query("SELECT status, message FROM status_current WHERE monitor_id = ?1")
            .bind(monitor_id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => {
                let status_str: String = r.get("status");
                let message: String = r.get("message");
                let status: Status =
                    serde_json::from_value(Value::String(status_str)).unwrap_or(Status::Unknown);
                Ok(Some((status, message)))
            }
            None => Ok(None),
        }
    }
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test store::`
Expected: PASS (4 tests).

- [ ] **Step 7: Full suite + commit**

Run: `cargo test`
Expected: PASS (all tasks' tests green).

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add SQLite store for monitors and current status"
```

---

## What this plan delivers

A tested backend library: the status/rollup model, a registry with three real check types (`http`, `tcp`, `frigate-camera`) that any monitor instance can use by config alone, and SQLite persistence. `cargo test` is green. No server or scheduler yet — that is Plan 2.

## Next plans (not in scope here)

- **Plan 2 — Runtime:** scheduler + debounce/hysteresis, `cert-expiry` and `ping` checks (with network integration tests), Home Assistant + ntfy notifiers, the axum JSON API (`/api/v1/status`, monitors CRUD, `/check-types`), and `main` wiring into a single binary.
- **Plan 3 — Web UI:** TypeScript + Preact dashboard and schema-driven CRUD forms, served as static assets by axum.
