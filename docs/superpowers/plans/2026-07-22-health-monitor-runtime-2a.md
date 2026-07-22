# Health Monitor — Runtime (Plan 2a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the Plan 1 library into a runnable single-binary daemon: a scheduler that runs the existing checks on their intervals with debounce, an axum JSON API to manage monitors and read status, env-based bootstrap config, and a `seed.sh` — so the user can `cargo run`, POST a few monitors, and `curl /api/v1/status` against real homelab services.

**Architecture:** The existing `homelab-health` lib gains a `main.rs` binary. `main` reads env config, connects the (Plan 1) SQLite `Store`, builds a `Registry::with_builtins()`, spawns a `Scheduler` background task, and serves an axum `Router`. The scheduler reads enabled monitors from the DB each tick, runs due ones through the registry, applies per-monitor `Debounce`, and persists committed status to `status_current`. The API is the only way to create/edit monitors; the DB is internal app state. Notifiers and extra check types are Plan 2b.

**Tech Stack:** Rust, tokio, axum, sqlx/SQLite, serde, reqwest (already present), tracing. Existing: status/report/check/store modules from Plan 1.

## Global Constraints

- Only capitalize the first letter of multi-letter acronyms (`HttpCheck`, `ApiState`, not `HTTPCheck`/`APIState`).
- Add dependencies with `cargo add` (never hand-edit versions).
- Format with `cargo +nightly fmt` before every commit.
- A check must never panic; library/handler paths must not `unwrap()` on fallible IO — map errors to a status code or `Status::Unknown`.
- **Env vars (bootstrap only):** `HEALTH_BIND` (default `0.0.0.0:8080`), `HEALTH_DB` (default `health.db`). No other config in env; no secrets in env.
- **API base path:** `/api/v1`. Monitor JSON fields are exactly: `name`, `type_id`, `config` (object), `interval_secs` (int), `enabled` (bool, default true).
- **Debounce default:** a state change commits after **2** consecutive matching results.
- Monitors are created/edited **only** via the API. The SQLite file is internal app state.

---

### Task 1: `Registry::with_builtins()`

**Files:**
- Modify: `src/check/mod.rs` (add the constructor + test)

**Interfaces:**
- Consumes: `Registry`, `HttpCheck`, `TcpCheck`, `FrigateCameraCheck` (all existing).
- Produces: `Registry::with_builtins() -> Registry` that has `http`, `tcp`, and `frigate-camera` registered.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `src/check/mod.rs`:
```rust
    #[test]
    fn with_builtins_registers_all_three() {
        let reg = Registry::with_builtins();
        assert!(reg.get("http").is_some());
        assert!(reg.get("tcp").is_some());
        assert!(reg.get("frigate-camera").is_some());
        assert_eq!(reg.schemas().len(), 3);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test check::tests::with_builtins`
Expected: FAIL — `no function with_builtins`.

- [ ] **Step 3: Implement**

Add to `impl Registry` in `src/check/mod.rs` (the module already has `use std::sync::Arc;`):
```rust
    /// A registry pre-loaded with every built-in check type.
    pub fn with_builtins() -> Self {
        let mut reg = Registry::new();
        reg.register(Arc::new(crate::check::http::HttpCheck));
        reg.register(Arc::new(crate::check::tcp::TcpCheck));
        reg.register(Arc::new(crate::check::frigate::FrigateCameraCheck));
        reg
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test check::tests::with_builtins`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add Registry::with_builtins"
```

---

### Task 2: Env-based config

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs` (add `pub mod config;`)

**Interfaces:**
- Produces:
  - `struct Config { bind: String, db_url: String }`
  - `Config::resolve(get: impl Fn(&str) -> Option<String>) -> Config` (testable core)
  - `Config::from_env() -> Config` (delegates to `resolve` reading `std::env::var`)
  - Defaults: `bind = "0.0.0.0:8080"`, `db_url` derived from `HEALTH_DB` (default `health.db`) as `format!("sqlite://{path}")`.

- [ ] **Step 1: Write the failing tests**

Create `src/config.rs`:
```rust
/// Bootstrap configuration. Only the essentials needed before the app can
/// serve requests come from the environment; everything else lives in the DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind: String,
    pub db_url: String,
}

impl Config {
    /// Resolve config from a generic getter (so it is testable without touching
    /// process-global env state).
    pub fn resolve(get: impl Fn(&str) -> Option<String>) -> Config {
        let bind = get("HEALTH_BIND").unwrap_or_else(|| "0.0.0.0:8080".to_string());
        let db_path = get("HEALTH_DB").unwrap_or_else(|| "health.db".to_string());
        Config {
            bind,
            db_url: format!("sqlite://{db_path}"),
        }
    }

    pub fn from_env() -> Config {
        Config::resolve(|k| std::env::var(k).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn getter(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn defaults_when_unset() {
        let cfg = Config::resolve(getter(&[]));
        assert_eq!(cfg.bind, "0.0.0.0:8080");
        assert_eq!(cfg.db_url, "sqlite://health.db");
    }

    #[test]
    fn reads_env_values() {
        let cfg = Config::resolve(getter(&[
            ("HEALTH_BIND", "127.0.0.1:9000"),
            ("HEALTH_DB", "/data/h.db"),
        ]));
        assert_eq!(cfg.bind, "127.0.0.1:9000");
        assert_eq!(cfg.db_url, "sqlite:///data/h.db");
    }
}
```

Add to `src/lib.rs`:
```rust
pub mod config;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test config::`
Expected: FAIL — module/type not found (before you add the file/mod line) or compile error.

- [ ] **Step 3: Implement** — already written above.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test config::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add env-based bootstrap config"
```

---

### Task 3: Store — serialize types, Clone, update/delete, file-DB connections

**Files:**
- Modify: `src/store.rs`

**Interfaces:**
- Consumes: existing `Store`, `Monitor`, `NewMonitor`.
- Produces:
  - `Store` derives `Clone`; `Monitor` derives `Serialize`; `NewMonitor` derives `Deserialize` (with `enabled` defaulting to true).
  - `Store::update_monitor(&self, id: i64, m: NewMonitor) -> Result<Option<Monitor>, sqlx::Error>`
  - `Store::delete_monitor(&self, id: i64) -> Result<bool, sqlx::Error>`
  - `Store::connect` allows multiple connections for file-backed DBs (keeps 1 for in-memory).

- [ ] **Step 1: Adjust derives and connection pool**

In `src/store.rs`:

Add `Serialize` where `Deserialize`/serde is imported. The top of the file currently has `use serde_json::Value;` — add:
```rust
use serde::{Deserialize, Serialize};
```

Change the `Store` struct to derive Clone:
```rust
#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}
```

Change `Monitor` to derive `Serialize` (keep existing derives):
```rust
#[derive(Debug, Clone, Serialize)]
pub struct Monitor {
```

Change `NewMonitor` to be deserializable with an `enabled` default:
```rust
#[derive(Debug, Deserialize)]
pub struct NewMonitor {
    pub name: String,
    pub type_id: String,
    pub config: Value,
    pub interval_secs: i64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}
```

In `Store::connect`, replace the `max_connections(1)` line so file DBs get a real pool while in-memory stays single-connection (the in-memory tests depend on max 1):
```rust
        let is_memory = url.contains(":memory:") || url.contains("mode=memory");
        let max_conns = if is_memory { 1 } else { 5 };
        let options = SqliteConnectOptions::from_str(url)?.create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conns)
            .connect_with(options)
            .await?;
```

- [ ] **Step 2: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/store.rs` (the `sample()` helper and `store()` helper already exist):
```rust
    #[tokio::test]
    async fn update_monitor_changes_fields() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        let updated = s
            .update_monitor(
                m.id,
                NewMonitor {
                    name: "Plex (edited)".into(),
                    type_id: "http".into(),
                    config: serde_json::json!({ "url": "http://plex.lan:32400" }),
                    interval_secs: 60,
                    enabled: false,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, "Plex (edited)");
        assert_eq!(updated.interval_secs, 60);
        assert!(!updated.enabled);
    }

    #[tokio::test]
    async fn update_missing_monitor_is_none() {
        let s = store().await;
        let res = s.update_monitor(999, sample()).await.unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn delete_monitor_removes_it() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        assert!(s.delete_monitor(m.id).await.unwrap());
        assert!(s.get_monitor(m.id).await.unwrap().is_none());
        assert!(!s.delete_monitor(m.id).await.unwrap());
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test store::`
Expected: FAIL — `no method update_monitor` / `delete_monitor`.

- [ ] **Step 4: Implement the methods**

Add to `impl Store` in `src/store.rs`:
```rust
    pub async fn update_monitor(
        &self,
        id: i64,
        m: NewMonitor,
    ) -> Result<Option<Monitor>, sqlx::Error> {
        let config_str = m.config.to_string();
        let rows = sqlx::query(
            "UPDATE monitors
             SET name = ?1, type_id = ?2, config_json = ?3, interval_secs = ?4, enabled = ?5
             WHERE id = ?6",
        )
        .bind(&m.name)
        .bind(&m.type_id)
        .bind(&config_str)
        .bind(m.interval_secs)
        .bind(m.enabled as i64)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();

        if rows == 0 {
            return Ok(None);
        }
        self.get_monitor(id).await
    }

    pub async fn delete_monitor(&self, id: i64) -> Result<bool, sqlx::Error> {
        let rows = sqlx::query("DELETE FROM monitors WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(rows > 0)
    }
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test store::`
Expected: PASS (all prior store tests + 3 new).

- [ ] **Step 6: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: store update/delete, serde derives, file-db pool"
```

---

### Task 4: Store — status read models

**Files:**
- Modify: `src/store.rs`

**Interfaces:**
- Consumes: `Monitor`, `Status`, `Component`, existing tables.
- Produces:
  - `struct MonitorStatus { monitor: Monitor, status: Option<Status>, message: Option<String>, components: Vec<Component>, updated_at: Option<String> }` (Serialize)
  - `Store::get_status(&self, id: i64) -> Result<Option<MonitorStatus>, sqlx::Error>`
  - `Store::list_status(&self) -> Result<Vec<MonitorStatus>, sqlx::Error>`

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/store.rs`:
```rust
    use crate::report::CheckReport;

    #[tokio::test]
    async fn get_status_is_none_status_before_first_check() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        let ms = s.get_status(m.id).await.unwrap().unwrap();
        assert_eq!(ms.monitor.id, m.id);
        assert!(ms.status.is_none());
        assert!(ms.components.is_empty());
    }

    #[tokio::test]
    async fn get_status_reflects_saved_report() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        let mut report = CheckReport::new(crate::status::Status::Critical, "HTTP 503");
        report
            .components
            .push(crate::report::Component::new("db", crate::status::Status::Critical, true, "down"));
        s.save_status(m.id, &report).await.unwrap();

        let ms = s.get_status(m.id).await.unwrap().unwrap();
        assert_eq!(ms.status, Some(crate::status::Status::Critical));
        assert_eq!(ms.message.as_deref(), Some("HTTP 503"));
        assert_eq!(ms.components.len(), 1);
        assert!(ms.updated_at.is_some());
    }

    #[tokio::test]
    async fn list_status_returns_every_monitor() {
        let s = store().await;
        s.create_monitor(sample()).await.unwrap();
        s.create_monitor(sample()).await.unwrap();
        let all = s.list_status().await.unwrap();
        assert_eq!(all.len(), 2);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test store::`
Expected: FAIL — `MonitorStatus` / `get_status` not found.

- [ ] **Step 3: Implement**

Add near the other structs in `src/store.rs`:
```rust
use crate::report::Component;
use crate::status::Status;

#[derive(Debug, Clone, Serialize)]
pub struct MonitorStatus {
    #[serde(flatten)]
    pub monitor: Monitor,
    pub status: Option<Status>,
    pub message: Option<String>,
    pub components: Vec<Component>,
    pub updated_at: Option<String>,
}
```

Add to `impl Store`:
```rust
    pub async fn get_status(&self, id: i64) -> Result<Option<MonitorStatus>, sqlx::Error> {
        let monitor = match self.get_monitor(id).await? {
            Some(m) => m,
            None => return Ok(None),
        };
        let row = sqlx::query(
            "SELECT status, message, components_json, updated_at
             FROM status_current WHERE monitor_id = ?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(Some(build_status(monitor, row)))
    }

    pub async fn list_status(&self) -> Result<Vec<MonitorStatus>, sqlx::Error> {
        let monitors = self.list_monitors().await?;
        let mut out = Vec::with_capacity(monitors.len());
        for monitor in monitors {
            let row = sqlx::query(
                "SELECT status, message, components_json, updated_at
                 FROM status_current WHERE monitor_id = ?1",
            )
            .bind(monitor.id)
            .fetch_optional(&self.pool)
            .await?;
            out.push(build_status(monitor, row));
        }
        Ok(out)
    }
```

Add this free function (below the `impl Store`), reusing `SqliteRow`/`Row` already imported:
```rust
fn build_status(monitor: Monitor, row: Option<SqliteRow>) -> MonitorStatus {
    match row {
        None => MonitorStatus {
            monitor,
            status: None,
            message: None,
            components: Vec::new(),
            updated_at: None,
        },
        Some(r) => {
            let status_str: String = r.try_get("status").unwrap_or_default();
            let status =
                serde_json::from_value(Value::String(status_str)).unwrap_or(Status::Unknown);
            let message: Option<String> = r.try_get("message").ok();
            let components_str: String = r.try_get("components_json").unwrap_or_default();
            let components: Vec<Component> =
                serde_json::from_str(&components_str).unwrap_or_default();
            let updated_at: Option<String> = r.try_get("updated_at").ok();
            MonitorStatus {
                monitor,
                status: Some(status),
                message,
                components,
                updated_at,
            }
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test store::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add MonitorStatus read models to store"
```

---

### Task 5: Debounce

**Files:**
- Create: `src/scheduler.rs`
- Modify: `src/lib.rs` (add `pub mod scheduler;`)

**Interfaces:**
- Consumes: `Status`.
- Produces: `struct Debounce` with `Debounce::new(threshold: u32)`, `record(&mut self, status: Status) -> Option<Status>` (Some when committed status changes), `committed(&self) -> Option<Status>`.

- [ ] **Step 1: Write the failing tests**

Create `src/scheduler.rs`:
```rust
use crate::status::Status;

/// Commits a status change only after `threshold` consecutive matching
/// observations, so a transient blip never flips the committed status.
pub struct Debounce {
    threshold: u32,
    committed: Option<Status>,
    candidate: Option<Status>,
    count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_after_threshold_consecutive() {
        let mut d = Debounce::new(2);
        assert_eq!(d.record(Status::Ok), None);
        assert_eq!(d.record(Status::Ok), Some(Status::Ok));
        assert_eq!(d.committed(), Some(Status::Ok));
    }

    #[test]
    fn single_blip_does_not_commit() {
        let mut d = Debounce::new(2);
        d.record(Status::Ok);
        d.record(Status::Ok); // committed Ok
        assert_eq!(d.record(Status::Critical), None); // blip
        assert_eq!(d.record(Status::Ok), None); // back to Ok, candidate cleared
        assert_eq!(d.committed(), Some(Status::Ok));
    }

    #[test]
    fn sustained_change_commits() {
        let mut d = Debounce::new(2);
        d.record(Status::Ok);
        d.record(Status::Ok);
        assert_eq!(d.record(Status::Critical), None);
        assert_eq!(d.record(Status::Critical), Some(Status::Critical));
    }

    #[test]
    fn threshold_one_commits_immediately() {
        let mut d = Debounce::new(1);
        assert_eq!(d.record(Status::Degraded), Some(Status::Degraded));
    }
}
```

Add to `src/lib.rs`:
```rust
pub mod scheduler;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test scheduler::tests`
Expected: FAIL — `no function new`.

- [ ] **Step 3: Implement**

Add to `src/scheduler.rs` (above the `tests` module):
```rust
impl Debounce {
    pub fn new(threshold: u32) -> Self {
        Debounce {
            threshold: threshold.max(1),
            committed: None,
            candidate: None,
            count: 0,
        }
    }

    /// Feed one raw observation. Returns Some(status) when the committed
    /// status changes as a result.
    pub fn record(&mut self, status: Status) -> Option<Status> {
        if self.committed == Some(status) {
            self.candidate = None;
            self.count = 0;
            return None;
        }
        if self.candidate == Some(status) {
            self.count += 1;
        } else {
            self.candidate = Some(status);
            self.count = 1;
        }
        if self.count >= self.threshold {
            self.committed = Some(status);
            self.candidate = None;
            self.count = 0;
            Some(status)
        } else {
            None
        }
    }

    pub fn committed(&self) -> Option<Status> {
        self.committed
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test scheduler::tests`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add Debounce hysteresis"
```

---

### Task 6: Scheduler

**Files:**
- Modify: `src/scheduler.rs`

**Interfaces:**
- Consumes: `Store`, `Monitor`, `Registry`, `CheckReport`, `Status`, `Debounce`.
- Produces:
  - `struct Scheduler { store: Store, registry: Arc<Registry>, threshold: u32, timeout: Duration, debouncers: HashMap<i64, Debounce> }`
  - `Scheduler::new(store: Store, registry: Arc<Registry>, threshold: u32) -> Scheduler`
  - `async fn run_and_record(&mut self, monitor: &Monitor) -> Result<CheckReport, sqlx::Error>` — runs the check (with timeout → Unknown), debounces, persists to `status_current` on a committed change.
  - `async fn run(self)` — the periodic loop (not unit-tested; wired in main).

- [ ] **Step 1: Add tracing dependency**

Run:
```bash
cargo add tracing
```

- [ ] **Step 2: Write the failing tests**

Add to the top of `src/scheduler.rs` (imports) and a new test in the `tests` module. Imports at the top of the file become:
```rust
use crate::check::Registry;
use crate::report::CheckReport;
use crate::status::Status;
use crate::store::{Monitor, Store};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
```

Add these tests to the `#[cfg(test)] mod tests` block:
```rust
    use crate::store::NewMonitor;
    use serde_json::json;

    async fn store_with_monitor(type_id: &str, config: serde_json::Value) -> (Store, Monitor) {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let m = store
            .create_monitor(NewMonitor {
                name: "t".into(),
                type_id: type_id.into(),
                config,
                interval_secs: 1,
                enabled: true,
            })
            .await
            .unwrap();
        (store, m)
    }

    #[tokio::test]
    async fn run_and_record_persists_after_threshold() {
        // tcp check against a closed port -> Critical.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let (store, m) =
            store_with_monitor("tcp", json!({ "host": "127.0.0.1", "port": port, "timeout_secs": 1 }))
                .await;
        let mut sched = Scheduler::new(store.clone(), Arc::new(Registry::with_builtins()), 2);

        // First observation: not yet committed, nothing persisted.
        sched.run_and_record(&m).await.unwrap();
        assert!(store.get_status(m.id).await.unwrap().unwrap().status.is_none());

        // Second consecutive Critical: commits and persists.
        sched.run_and_record(&m).await.unwrap();
        assert_eq!(
            store.get_status(m.id).await.unwrap().unwrap().status,
            Some(Status::Critical)
        );
    }

    #[tokio::test]
    async fn unknown_type_records_unknown() {
        let (store, m) = store_with_monitor("does-not-exist", json!({})).await;
        let mut sched = Scheduler::new(store.clone(), Arc::new(Registry::with_builtins()), 1);
        let report = sched.run_and_record(&m).await.unwrap();
        assert_eq!(report.status, Status::Unknown);
        assert_eq!(
            store.get_status(m.id).await.unwrap().unwrap().status,
            Some(Status::Unknown)
        );
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test scheduler::tests::run_and_record`
Expected: FAIL — `no function new` for Scheduler.

- [ ] **Step 4: Implement**

Add to `src/scheduler.rs` (above the `tests` module, after the `Debounce` impl):
```rust
pub struct Scheduler {
    store: Store,
    registry: Arc<Registry>,
    threshold: u32,
    timeout: Duration,
    debouncers: HashMap<i64, Debounce>,
}

impl Scheduler {
    pub fn new(store: Store, registry: Arc<Registry>, threshold: u32) -> Scheduler {
        Scheduler {
            store,
            registry,
            threshold,
            timeout: Duration::from_secs(30),
            debouncers: HashMap::new(),
        }
    }

    async fn run_check(&self, monitor: &Monitor) -> CheckReport {
        let fut = self.registry.run(&monitor.type_id, &monitor.config);
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(report) => report,
            Err(_) => CheckReport::new(Status::Unknown, "check timed out"),
        }
    }

    /// Run one check, feed the result through the monitor's debounce, and
    /// persist to status_current when the committed status changes.
    pub async fn run_and_record(
        &mut self,
        monitor: &Monitor,
    ) -> Result<CheckReport, sqlx::Error> {
        let report = self.run_check(monitor).await;
        let threshold = self.threshold;
        let debounce = self
            .debouncers
            .entry(monitor.id)
            .or_insert_with(|| Debounce::new(threshold));
        if debounce.record(report.status).is_some() {
            self.store.save_status(monitor.id, &report).await?;
        }
        Ok(report)
    }

    /// Periodic loop: every second, run each enabled monitor whose interval has
    /// elapsed. Reads monitors from the DB each pass so API edits take effect.
    pub async fn run(mut self) {
        let mut last_run: HashMap<i64, tokio::time::Instant> = HashMap::new();
        loop {
            match self.store.list_monitors().await {
                Ok(monitors) => {
                    let now = tokio::time::Instant::now();
                    for monitor in monitors.iter().filter(|m| m.enabled) {
                        let interval = Duration::from_secs(monitor.interval_secs.max(1) as u64);
                        let due = last_run
                            .get(&monitor.id)
                            .map_or(true, |t| now.duration_since(*t) >= interval);
                        if due {
                            last_run.insert(monitor.id, now);
                            if let Err(e) = self.run_and_record(monitor).await {
                                tracing::error!("check '{}' failed to persist: {e}", monitor.name);
                            }
                        }
                    }
                }
                Err(e) => tracing::error!("scheduler could not list monitors: {e}"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test scheduler::`
Expected: PASS (Debounce tests + 2 scheduler tests).

- [ ] **Step 6: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: add Scheduler with debounce and periodic loop"
```

---

### Task 7: API — state, router, monitors CRUD + check-types

**Files:**
- Create: `src/api.rs`
- Modify: `src/lib.rs` (add `pub mod api;`)

**Interfaces:**
- Consumes: `Store`, `NewMonitor`, `Monitor`, `Registry`.
- Produces:
  - `#[derive(Clone)] struct ApiState { store: Store, registry: Arc<Registry> }`
  - `fn build_app(state: ApiState) -> axum::Router`
  - Routes: `GET /api/v1/check-types`, `GET /api/v1/monitors`, `POST /api/v1/monitors`, `PUT /api/v1/monitors/:id`, `DELETE /api/v1/monitors/:id`.

- [ ] **Step 1: Add axum**

Run:
```bash
cargo add axum
```

- [ ] **Step 2: Write the failing tests**

Create `src/api.rs`:
```rust
use crate::check::{ConfigSchema, Registry};
use crate::store::{Monitor, NewMonitor, Store};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone)]
pub struct ApiState {
    pub store: Store,
    pub registry: Arc<Registry>,
}

fn internal(e: sqlx::Error) -> StatusCode {
    tracing::error!("db error: {e}");
    StatusCode::INTERNAL_SERVER_ERROR
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn spawn() -> (String, Store) {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let state = ApiState {
            store: store.clone(),
            registry: Arc::new(Registry::with_builtins()),
        };
        let app = build_app(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), store)
    }

    #[tokio::test]
    async fn check_types_lists_builtins() {
        let (base, _store) = spawn().await;
        let body: Value = reqwest::get(format!("{base}/api/v1/check-types"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let arr = body.as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[tokio::test]
    async fn create_then_list_and_update_and_delete() {
        let (base, _store) = spawn().await;
        let client = reqwest::Client::new();

        // Create
        let created: Monitor = client
            .post(format!("{base}/api/v1/monitors"))
            .json(&json!({
                "name": "Plex",
                "type_id": "http",
                "config": { "url": "http://plex.lan" },
                "interval_secs": 30
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(created.id > 0);
        assert!(created.enabled); // defaulted true

        // List
        let list: Vec<Monitor> = client
            .get(format!("{base}/api/v1/monitors"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(list.len(), 1);

        // Update
        let updated: Monitor = client
            .put(format!("{base}/api/v1/monitors/{}", created.id))
            .json(&json!({
                "name": "Plex2",
                "type_id": "http",
                "config": { "url": "http://plex.lan" },
                "interval_secs": 60,
                "enabled": false
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(updated.name, "Plex2");

        // Delete
        let del = client
            .delete(format!("{base}/api/v1/monitors/{}", created.id))
            .send()
            .await
            .unwrap();
        assert_eq!(del.status(), 204);
    }

    #[tokio::test]
    async fn update_missing_returns_404() {
        let (base, _store) = spawn().await;
        let resp = reqwest::Client::new()
            .put(format!("{base}/api/v1/monitors/999"))
            .json(&json!({
                "name": "x", "type_id": "http",
                "config": { "url": "http://x" }, "interval_secs": 30
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }
}
```

Add to `src/lib.rs`:
```rust
pub mod api;
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test api::`
Expected: FAIL — `build_app` not found.

- [ ] **Step 4: Implement handlers + router**

Add to `src/api.rs` (above the `tests` module):
```rust
pub fn build_app(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/check-types", get(check_types))
        .route("/api/v1/monitors", get(list_monitors).post(create_monitor))
        .route(
            "/api/v1/monitors/{id}",
            axum::routing::put(update_monitor).delete(delete_monitor),
        )
        .with_state(state)
}

async fn check_types(State(state): State<ApiState>) -> Json<Value> {
    let schemas: Vec<Value> = state
        .registry
        .schemas()
        .into_iter()
        .map(|(type_id, schema): (&str, ConfigSchema)| {
            json!({ "type_id": type_id, "schema": schema })
        })
        .collect();
    Json(json!(schemas))
}

async fn list_monitors(
    State(state): State<ApiState>,
) -> Result<Json<Vec<Monitor>>, StatusCode> {
    let monitors = state.store.list_monitors().await.map_err(internal)?;
    Ok(Json(monitors))
}

async fn create_monitor(
    State(state): State<ApiState>,
    Json(body): Json<NewMonitor>,
) -> Result<(StatusCode, Json<Monitor>), StatusCode> {
    let monitor = state.store.create_monitor(body).await.map_err(internal)?;
    Ok((StatusCode::CREATED, Json(monitor)))
}

async fn update_monitor(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
    Json(body): Json<NewMonitor>,
) -> Result<Json<Monitor>, StatusCode> {
    match state.store.update_monitor(id, body).await.map_err(internal)? {
        Some(m) => Ok(Json(m)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn delete_monitor(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    if state.store.delete_monitor(id).await.map_err(internal)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
```

Note: `ConfigSchema` must be `Serialize` (it is, from Plan 1) for the `check_types` handler.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test api::`
Expected: PASS (3 tests). The route path uses axum 0.8+ `{id}` capture syntax. (If `cargo add` pinned an older axum <0.8, that version instead wants `:id` — switch if the router panics at startup with a path error.)

- [ ] **Step 6: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: axum API for monitors CRUD and check-types"
```

---

### Task 8: API — status endpoints + run-now

**Files:**
- Modify: `src/api.rs`

**Interfaces:**
- Consumes: `ApiState`, `Store::list_status`, `Store::get_status`, `Store::get_monitor`, `Registry::run`, `Store::save_status`.
- Produces routes: `GET /api/v1/status`, `GET /api/v1/status/:id`, `POST /api/v1/monitors/:id/run` (runs the check now, persists status_current, returns the report).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/api.rs`:
```rust
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn status_lists_monitors_unknown_before_check() {
        let (base, store) = spawn().await;
        store
            .create_monitor(NewMonitor {
                name: "m".into(),
                type_id: "http".into(),
                config: json!({ "url": "http://x" }),
                interval_secs: 30,
                enabled: true,
            })
            .await
            .unwrap();
        let body: Value = reqwest::get(format!("{base}/api/v1/status"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let arr = body.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        // status is null until first check
        assert!(arr[0]["status"].is_null());
        assert_eq!(arr[0]["name"], "m");
    }

    #[tokio::test]
    async fn run_now_executes_and_persists() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;

        let (base, store) = spawn().await;
        let m = store
            .create_monitor(NewMonitor {
                name: "m".into(),
                type_id: "http".into(),
                config: json!({ "url": mock.uri() }),
                interval_secs: 30,
                enabled: true,
            })
            .await
            .unwrap();

        let report: Value = reqwest::Client::new()
            .post(format!("{base}/api/v1/monitors/{}/run", m.id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(report["status"], "ok");

        // persisted
        let got = store.get_status(m.id).await.unwrap().unwrap();
        assert_eq!(got.status, Some(crate::status::Status::Ok));
    }

    #[tokio::test]
    async fn run_now_missing_monitor_404() {
        let (base, _store) = spawn().await;
        let resp = reqwest::Client::new()
            .post(format!("{base}/api/v1/monitors/999/run"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test api::tests::status_lists`
Expected: FAIL — route not found (404) / handler missing.

- [ ] **Step 3: Implement**

Add the three routes to `build_app` (chain before `.with_state(state)`):
```rust
        .route("/api/v1/status", get(list_status))
        .route("/api/v1/status/{id}", get(get_status))
        .route("/api/v1/monitors/{id}/run", post(run_now))
```
(Uses the same `{id}` capture syntax as Task 7. If Task 7 had to fall back to `:id` for an older axum, use `:id` here too.)

Add the handlers (above the `tests` module):
```rust
use crate::store::MonitorStatus;

async fn list_status(
    State(state): State<ApiState>,
) -> Result<Json<Vec<MonitorStatus>>, StatusCode> {
    let all = state.store.list_status().await.map_err(internal)?;
    Ok(Json(all))
}

async fn get_status(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<MonitorStatus>, StatusCode> {
    match state.store.get_status(id).await.map_err(internal)? {
        Some(ms) => Ok(Json(ms)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn run_now(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<crate::report::CheckReport>, StatusCode> {
    let monitor = match state.store.get_monitor(id).await.map_err(internal)? {
        Some(m) => m,
        None => return Err(StatusCode::NOT_FOUND),
    };
    let report = state.registry.run(&monitor.type_id, &monitor.config).await;
    state.store.save_status(id, &report).await.map_err(internal)?;
    Ok(Json(report))
}
```
Note: `CheckReport` must be `Serialize` (it is, from Plan 1).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test api::`
Expected: PASS (all Task 7 + 3 new).

- [ ] **Step 5: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: status endpoints and run-now"
```

---

### Task 9: main binary + seed.sh + smoke verification

**Files:**
- Create: `src/main.rs`
- Create: `seed.sh`
- Modify: `README.md` (create if absent) — brief run instructions

**Interfaces:**
- Consumes: `Config`, `Store`, `Registry::with_builtins`, `Scheduler`, `api::build_app`, `api::ApiState`.
- Produces: a runnable binary `homelab-health`.

- [ ] **Step 1: Add tracing-subscriber**

Run:
```bash
cargo add tracing-subscriber --features env-filter
```

- [ ] **Step 2: Write main.rs**

Create `src/main.rs`:
```rust
use homelab_health::api::{build_app, ApiState};
use homelab_health::check::Registry;
use homelab_health::config::Config;
use homelab_health::scheduler::Scheduler;
use homelab_health::store::Store;
use std::sync::Arc;

const DEBOUNCE_THRESHOLD: u32 = 2;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env();
    tracing::info!("connecting to {}", config.db_url);
    let store = Store::connect(&config.db_url).await?;
    let registry = Arc::new(Registry::with_builtins());

    // Background scheduler.
    let scheduler = Scheduler::new(store.clone(), registry.clone(), DEBOUNCE_THRESHOLD);
    tokio::spawn(scheduler.run());

    // HTTP API.
    let state = ApiState {
        store,
        registry,
    };
    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("listening on {}", config.bind);
    axum::serve(listener, app).await?;
    Ok(())
}
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build`
Expected: compiles; produces a `homelab-health` binary. (Cargo builds `src/main.rs` as the package binary alongside the existing lib.)

- [ ] **Step 4: Write seed.sh**

Create `seed.sh` (chmod +x):
```bash
#!/usr/bin/env bash
# Seed a few monitors via the API for live testing. Override BASE / targets as needed.
set -euo pipefail
BASE="${BASE:-http://localhost:8080}"

post() {
  curl -sS -X POST "$BASE/api/v1/monitors" \
    -H 'content-type: application/json' \
    -d "$1" | jq -c . || echo "  (is the daemon running? is jq installed?)"
}

# HTTP check against Plex web UI (adjust host).
post '{"name":"Plex","type_id":"http","interval_secs":30,"config":{"url":"http://plex.lan:32400/web/index.html"}}'

# TCP check against a service port (adjust host/port).
post '{"name":"Unraid SMB","type_id":"tcp","interval_secs":30,"config":{"host":"unraid.lan","port":445}}'

# Frigate per-camera check (adjust base_url).
post '{"name":"Frigate","type_id":"frigate-camera","interval_secs":60,"config":{"base_url":"http://frigate.lan:5000"}}'

echo "Seeded. Now: curl -s $BASE/api/v1/status | jq"
```

- [ ] **Step 5: Manual smoke verification**

This wiring cannot be meaningfully unit-tested (the API + scheduler are already covered by their own tests). Verify the assembled binary by hand:

Run:
```bash
cargo run &
sleep 2
curl -s localhost:8080/api/v1/check-types | jq 'length'   # expect 3
curl -s -X POST localhost:8080/api/v1/monitors -H 'content-type: application/json' \
  -d '{"name":"self","type_id":"tcp","interval_secs":5,"config":{"host":"127.0.0.1","port":8080}}' | jq
sleep 7
curl -s localhost:8080/api/v1/status | jq '.[0].status'   # expect "ok" after the scheduler runs it
kill %1
```
Expected: check-types returns 3; the self-monitor shows `"ok"` after ~2 intervals (debounce threshold 2). Record the observed output in the report.

- [ ] **Step 6: Write README run instructions**

Create/append `README.md`:
```markdown
# homelab-health

Self-hosted service health monitor.

## Run locally

```bash
cargo run
# env (optional): HEALTH_BIND=0.0.0.0:8080  HEALTH_DB=health.db
./seed.sh                      # seed a few monitors (edit hosts first)
curl -s localhost:8080/api/v1/status | jq
```

Monitors are managed only through the API (`/api/v1/monitors`); the SQLite DB
is internal app state. See `docs/health-endpoint-contract.md` for the
service-side `/health` contract.
```

- [ ] **Step 7: Commit**

```bash
cargo +nightly fmt
git add -A
git commit -m "feat: main binary, seed.sh, run docs"
```

---

## What this plan delivers

A runnable single binary: `cargo run` starts the scheduler (running `http`/`tcp`/`frigate` checks on their intervals with debounce) and serves the JSON API. Monitors are created via `POST /api/v1/monitors`; `GET /api/v1/status` reports current per-monitor (and per-component) health. Full unit/integration coverage on config, store, debounce, scheduler, and every API route; `main` verified by a documented smoke test.

## Next (Plan 2b)

Notifier settings-in-DB + REST config + per-monitor selection (which notifiers, min severity); Home Assistant + ntfy notifiers fired on committed transitions; and the `cert-expiry`, `ping`, and `json-health` check types. Also deferred: concurrent (non-sequential) scheduler execution, and status history/timeline.
