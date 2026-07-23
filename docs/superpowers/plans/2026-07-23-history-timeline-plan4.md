# History / Timeline (Plan 4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`).

**Goal:** Record monitor history and surface it: an uptime timeline (red/green over 24h/7d) built from committed transitions, and a scroll-back forensic history panel built from raw per-check samples (7-day retention). Serves "look back after a false-negative and know what to tweak."

**Architecture:** Two new SQLite tables — `status_transitions` (one row per committed change, kept indefinitely) and `check_samples` (one row per check run, pruned after N days). The scheduler records a sample every run and a transition on each committed change, and prunes periodically. A pure Rust `compute_uptime` turns transitions into segments + percentages. Two new API endpoints feed a UI timeline bar + history panel. `at` columns are unix epoch seconds for easy math.

**Tech Stack:** Rust (sqlx, axum), Preact/TS. All existing.

## Global Constraints
- Only capitalize the first letter of multi-letter acronyms.
- TypeScript (not JS) for UI. Rust formatted with `cargo +nightly fmt`.
- Add deps with `cargo add` (none expected). No panics in library/handler paths.
- COMMIT WITH EXPLICIT PATHS (never `git add -A`; tracked `seed.sh` must not be swept — `git status` before each commit).
- Migrations are idempotent (`CREATE TABLE IF NOT EXISTS`), run on every `connect`.
- Retention default **7 days**; transitions kept indefinitely.

---

### Task 1: Store — history tables + record/query/prune

**Files:** Create `migrations/0002_history.sql`; modify `src/store.rs`, `src/status.rs`.

**Interfaces produced:**
- `Status::as_str(&self) -> &'static str` and `Status::from_db(&str) -> Status` (snake_case; unknown/unrecognized → `Unknown`).
- `struct Sample { status: Status, message: String, components: Vec<Component>, at: i64 }` (Serialize).
- `Store::record_sample(&self, monitor_id: i64, report: &CheckReport) -> Result<(), sqlx::Error>`
- `Store::record_transition(&self, monitor_id: i64, status: Status, message: &str) -> Result<(), sqlx::Error>`
- `Store::prune_samples(&self, retention_days: i64) -> Result<u64, sqlx::Error>` (rows deleted)
- `Store::get_samples(&self, monitor_id: i64, limit: i64) -> Result<Vec<Sample>, sqlx::Error>` (newest first)
- `Store::get_transitions_since(&self, monitor_id: i64, since: i64) -> Result<Vec<(Status, i64)>, sqlx::Error>` (asc by at)
- `Store::status_at(&self, monitor_id: i64, at: i64) -> Result<Option<Status>, sqlx::Error>` (last transition with `at <= given`)

- [ ] **Step 1: Add Status db helpers**

In `src/status.rs`, add to `impl Status`:
```rust
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Degraded => "degraded",
            Status::Critical => "critical",
            Status::Unknown => "unknown",
        }
    }

    pub fn from_db(s: &str) -> Status {
        match s {
            "ok" => Status::Ok,
            "degraded" => Status::Degraded,
            "critical" => Status::Critical,
            _ => Status::Unknown,
        }
    }
```
Add a test asserting round-trip for all four and that garbage → Unknown.

- [ ] **Step 2: Migration**

Create `migrations/0002_history.sql`:
```sql
CREATE TABLE IF NOT EXISTS status_transitions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    monitor_id INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    status     TEXT    NOT NULL,
    message    TEXT    NOT NULL,
    at         INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_transitions_monitor_at ON status_transitions(monitor_id, at);

CREATE TABLE IF NOT EXISTS check_samples (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    monitor_id      INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    status          TEXT    NOT NULL,
    message         TEXT    NOT NULL,
    components_json TEXT    NOT NULL,
    at              INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_samples_monitor_at ON check_samples(monitor_id, at);
```

In `Store::connect`, after the existing `sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))...` line, add:
```rust
        sqlx::raw_sql(include_str!("../migrations/0002_history.sql"))
            .execute(&pool)
            .await?;
```

- [ ] **Step 3: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `src/store.rs`:
```rust
    #[tokio::test]
    async fn records_and_reads_samples() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        s.record_sample(m.id, &CheckReport::new(Status::Critical, "boom"))
            .await
            .unwrap();
        s.record_sample(m.id, &CheckReport::ok("fine")).await.unwrap();
        let rows = s.get_samples(m.id, 10).await.unwrap();
        assert_eq!(rows.len(), 2);
        // newest first
        assert_eq!(rows[0].status, Status::Ok);
    }

    #[tokio::test]
    async fn records_transitions_and_status_at() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        s.record_transition(m.id, Status::Ok, "up").await.unwrap();
        s.record_transition(m.id, Status::Critical, "down").await.unwrap();
        let since = s.get_transitions_since(m.id, 0).await.unwrap();
        assert_eq!(since.len(), 2);
        assert_eq!(since[0].0, Status::Ok); // ascending
        // status_at "now+large" should be the latest (Critical)
        let at_now = s.status_at(m.id, 9_999_999_999).await.unwrap();
        assert_eq!(at_now, Some(Status::Critical));
    }

    #[tokio::test]
    async fn prune_removes_old_samples_only() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        // an old sample (10 days ago) and a fresh one
        s.insert_sample_at(m.id, Status::Ok, "old", 10).await; // helper below (test-only)
        s.record_sample(m.id, &CheckReport::ok("new")).await.unwrap();
        let deleted = s.prune_samples(7).await.unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(s.get_samples(m.id, 10).await.unwrap().len(), 1);
    }
```
Add a test-only helper on `Store` (behind `#[cfg(test)]`) to insert a sample at N days ago:
```rust
    #[cfg(test)]
    async fn insert_sample_at(&self, monitor_id: i64, status: Status, message: &str, days_ago: i64) {
        sqlx::query(
            "INSERT INTO check_samples (monitor_id, status, message, components_json, at)
             VALUES (?1, ?2, ?3, '[]', strftime('%s','now') - ?4 * 86400)",
        )
        .bind(monitor_id)
        .bind(status.as_str())
        .bind(message)
        .bind(days_ago)
        .execute(&self.pool)
        .await
        .unwrap();
    }
```

- [ ] **Step 4: Run — fails**

Run: `cargo test store::` → FAIL (methods missing).

- [ ] **Step 5: Implement**

Add the `Sample` struct near the other store structs (with `use crate::report::Component; use crate::status::Status;` already imported):
```rust
#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub status: Status,
    pub message: String,
    pub components: Vec<Component>,
    pub at: i64,
}
```
Add to `impl Store`:
```rust
    pub async fn record_sample(
        &self,
        monitor_id: i64,
        report: &CheckReport,
    ) -> Result<(), sqlx::Error> {
        let components = serde_json::to_string(&report.components).unwrap_or_else(|_| "[]".into());
        sqlx::query(
            "INSERT INTO check_samples (monitor_id, status, message, components_json)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(monitor_id)
        .bind(report.status.as_str())
        .bind(&report.message)
        .bind(components)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_transition(
        &self,
        monitor_id: i64,
        status: Status,
        message: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO status_transitions (monitor_id, status, message) VALUES (?1, ?2, ?3)",
        )
        .bind(monitor_id)
        .bind(status.as_str())
        .bind(message)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn prune_samples(&self, retention_days: i64) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "DELETE FROM check_samples WHERE at < strftime('%s','now') - ?1 * 86400",
        )
        .bind(retention_days)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    pub async fn get_samples(&self, monitor_id: i64, limit: i64) -> Result<Vec<Sample>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT status, message, components_json, at FROM check_samples
             WHERE monitor_id = ?1 ORDER BY at DESC, id DESC LIMIT ?2",
        )
        .bind(monitor_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let status_str: String = r.try_get("status").unwrap_or_default();
                let components_str: String = r.try_get("components_json").unwrap_or_default();
                Sample {
                    status: Status::from_db(&status_str),
                    message: r.try_get("message").unwrap_or_default(),
                    components: serde_json::from_str(&components_str).unwrap_or_default(),
                    at: r.try_get("at").unwrap_or_default(),
                }
            })
            .collect())
    }

    pub async fn get_transitions_since(
        &self,
        monitor_id: i64,
        since: i64,
    ) -> Result<Vec<(Status, i64)>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT status, at FROM status_transitions
             WHERE monitor_id = ?1 AND at > ?2 ORDER BY at ASC, id ASC",
        )
        .bind(monitor_id)
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let s: String = r.try_get("status").unwrap_or_default();
                (Status::from_db(&s), r.try_get("at").unwrap_or_default())
            })
            .collect())
    }

    pub async fn status_at(&self, monitor_id: i64, at: i64) -> Result<Option<Status>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT status FROM status_transitions
             WHERE monitor_id = ?1 AND at <= ?2 ORDER BY at DESC, id DESC LIMIT 1",
        )
        .bind(monitor_id)
        .bind(at)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            let s: String = r.try_get("status").unwrap_or_default();
            Status::from_db(&s)
        }))
    }
```

- [ ] **Step 6: Run tests → pass. Commit.**

```bash
cargo test store::
cargo test
cargo +nightly fmt
git add migrations/0002_history.sql src/store.rs src/status.rs
git commit -m "feat: history tables + record/query/prune in store"
```

---

### Task 2: Uptime computation (pure function)

**Files:** Create `src/uptime.rs`; add `pub mod uptime;` to `src/lib.rs`.

**Interfaces produced:**
- `struct Segment { status: Status, start: i64, end: i64 }` (Serialize)
- `struct Uptime { window_secs, ok_secs, degraded_secs, critical_secs, unknown_secs, percent_ok: f64, segments: Vec<Segment> }` (Serialize)
- `fn compute_uptime(prior: Status, transitions: &[(Status, i64)], window_start: i64, now: i64) -> Uptime`

- [ ] **Step 1: Write the failing tests**

Create `src/uptime.rs`:
```rust
use crate::status::Status;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub status: Status,
    pub start: i64,
    pub end: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Uptime {
    pub window_secs: i64,
    pub ok_secs: i64,
    pub degraded_secs: i64,
    pub critical_secs: i64,
    pub unknown_secs: i64,
    pub percent_ok: f64,
    pub segments: Vec<Segment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ok_whole_window_is_100() {
        // prior Ok, no transitions in window; window 100s.
        let u = compute_uptime(Status::Ok, &[], 0, 100);
        assert_eq!(u.ok_secs, 100);
        assert_eq!(u.percent_ok, 100.0);
        assert_eq!(u.segments.len(), 1);
    }

    #[test]
    fn one_outage_splits_and_scores() {
        // Ok from 0..60, Critical 60..100 (transition to Critical at t=60).
        let u = compute_uptime(Status::Ok, &[(Status::Critical, 60)], 0, 100);
        assert_eq!(u.ok_secs, 60);
        assert_eq!(u.critical_secs, 40);
        assert_eq!(u.percent_ok, 60.0);
        assert_eq!(u.segments.len(), 2);
        assert_eq!(u.segments[0].status, Status::Ok);
        assert_eq!(u.segments[1].status, Status::Critical);
    }

    #[test]
    fn no_prior_history_is_unknown() {
        let u = compute_uptime(Status::Unknown, &[], 0, 50);
        assert_eq!(u.unknown_secs, 50);
        assert_eq!(u.percent_ok, 0.0);
    }
}
```
Add `pub mod uptime;` to `src/lib.rs`.

- [ ] **Step 2: Run → fails (no compute_uptime). Step 3: Implement.**

Add to `src/uptime.rs` (above the tests):
```rust
pub fn compute_uptime(
    prior: Status,
    transitions: &[(Status, i64)],
    window_start: i64,
    now: i64,
) -> Uptime {
    let mut ok = 0i64;
    let mut degraded = 0i64;
    let mut critical = 0i64;
    let mut unknown = 0i64;
    let mut segments = Vec::new();

    let mut current = prior;
    let mut seg_start = window_start;

    let mut add = |status: Status, start: i64, end: i64| {
        let dur = (end - start).max(0);
        match status {
            Status::Ok => ok += dur,
            Status::Degraded => degraded += dur,
            Status::Critical => critical += dur,
            Status::Unknown => unknown += dur,
        }
        segments.push(Segment { status, start, end });
    };

    for (status, at) in transitions {
        let at = (*at).clamp(window_start, now);
        if at > seg_start {
            add(current, seg_start, at);
            seg_start = at;
        }
        current = *status;
    }
    add(current, seg_start, now);

    let window_secs = (now - window_start).max(0);
    let percent_ok = if window_secs > 0 {
        ok as f64 / window_secs as f64 * 100.0
    } else {
        0.0
    };

    Uptime {
        window_secs,
        ok_secs: ok,
        degraded_secs: degraded,
        critical_secs: critical,
        unknown_secs: unknown,
        percent_ok,
        segments,
    }
}
```

- [ ] **Step 4: tests pass. Commit.**

```bash
cargo test uptime::
cargo +nightly fmt
git add src/uptime.rs src/lib.rs
git commit -m "feat: pure compute_uptime over transitions"
```

---

### Task 3: Scheduler records history + prunes

**Files:** Modify `src/scheduler.rs`.

**Interfaces:** `Scheduler` gains a `retention_days: i64` field (default 7) and a builder `retention_days(mut self, n: i64) -> Self`. `run_and_record` records a sample every run and a transition on committed change; `run` prunes periodically.

- [ ] **Step 1: Write the failing tests**

Add to the scheduler `tests` module:
```rust
    #[tokio::test]
    async fn run_and_record_writes_a_sample_every_run() {
        let (store, m) = store_with_monitor("tcp", json!({ "host": "127.0.0.1", "port": 1, "timeout_secs": 1 })).await;
        let mut sched = Scheduler::new(store.clone(), Arc::new(Registry::with_builtins()), 2);
        sched.run_and_record(&m).await.unwrap();
        sched.run_and_record(&m).await.unwrap();
        assert_eq!(store.get_samples(m.id, 10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn committed_change_writes_a_transition() {
        let (store, m) = store_with_monitor("tcp", json!({ "host": "127.0.0.1", "port": 1, "timeout_secs": 1 })).await;
        let mut sched = Scheduler::new(store.clone(), Arc::new(Registry::with_builtins()), 2);
        sched.run_and_record(&m).await.unwrap(); // 1st critical, not committed
        assert_eq!(store.get_transitions_since(m.id, 0).await.unwrap().len(), 0);
        sched.run_and_record(&m).await.unwrap(); // 2nd -> commit
        assert_eq!(store.get_transitions_since(m.id, 0).await.unwrap().len(), 1);
    }
```

- [ ] **Step 2: Run → fails. Step 3: Implement.**

Add the field to `struct Scheduler`:
```rust
    retention_days: i64,
```
In `Scheduler::new`, initialize `retention_days: 7,`. Add the builder (after `new`):
```rust
    pub fn retention_days(mut self, days: i64) -> Self {
        self.retention_days = days;
        self
    }
```
Update `run_and_record` to record sample + transition:
```rust
    pub async fn run_and_record(
        &mut self,
        monitor: &Monitor,
    ) -> Result<CheckReport, sqlx::Error> {
        let report = self.run_check(monitor).await;
        self.store.record_sample(monitor.id, &report).await?;
        let threshold = self.threshold;
        let debounce = self
            .debouncers
            .entry(monitor.id)
            .or_insert_with(|| Debounce::new(threshold));
        if let Some(committed) = debounce.record(report.status) {
            self.store.save_status(monitor.id, &report).await?;
            self.store
                .record_transition(monitor.id, committed, &report.message)
                .await?;
        }
        Ok(report)
    }
```
In `run`, prune on start and hourly. Before the loop:
```rust
        if let Err(e) = self.store.prune_samples(self.retention_days).await {
            tracing::error!("prune failed: {e}");
        }
        let mut last_prune = tokio::time::Instant::now();
```
Inside the loop (after processing monitors, before the sleep):
```rust
            if now.duration_since(last_prune) >= Duration::from_secs(3600) {
                if let Err(e) = self.store.prune_samples(self.retention_days).await {
                    tracing::error!("prune failed: {e}");
                }
                last_prune = now;
            }
```
(`now` already exists in the loop as `tokio::time::Instant::now()`.)

- [ ] **Step 4: tests pass (full suite). Commit.**

```bash
cargo test scheduler::
cargo test
cargo +nightly fmt
git add src/scheduler.rs
git commit -m "feat: scheduler records samples + transitions and prunes"
```

---

### Task 4: History + uptime API endpoints

**Files:** Modify `src/api.rs`.

**Interfaces / routes:**
- `GET /api/v1/monitors/{id}/history?limit=N` (default 100, cap 500) → `Sample[]` (404 if monitor missing).
- `GET /api/v1/monitors/{id}/uptime?window=SECONDS` (default 86400) → `Uptime` (404 if missing). Handler computes `now = unix epoch`, `window_start = now - window`, `prior = store.status_at(id, window_start)` (None → Unknown), `transitions = store.get_transitions_since(id, window_start)`, then `compute_uptime(...)`.

- [ ] **Step 1: Write the failing tests**

Add to the api `tests` module (uses the existing `spawn()` returning `(base, store)`):
```rust
    #[tokio::test]
    async fn history_endpoint_returns_samples() {
        let (base, store) = spawn().await;
        let m = store.create_monitor(NewMonitor {
            name: "m".into(), type_id: "http".into(),
            config: json!({ "url": "http://x" }), interval_secs: 30, enabled: true,
        }).await.unwrap();
        store.record_sample(m.id, &crate::report::CheckReport::ok("hi")).await.unwrap();
        let body: Value = reqwest::get(format!("{base}/api/v1/monitors/{}/history", m.id))
            .await.unwrap().json().await.unwrap();
        assert_eq!(body.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn uptime_endpoint_computes_percent() {
        let (base, store) = spawn().await;
        let m = store.create_monitor(NewMonitor {
            name: "m".into(), type_id: "http".into(),
            config: json!({ "url": "http://x" }), interval_secs: 30, enabled: true,
        }).await.unwrap();
        store.record_transition(m.id, crate::status::Status::Ok, "up").await.unwrap();
        let body: Value = reqwest::get(format!("{base}/api/v1/monitors/{}/uptime?window=3600", m.id))
            .await.unwrap().json().await.unwrap();
        assert!(body["percent_ok"].as_f64().unwrap() > 0.0);
    }

    #[tokio::test]
    async fn history_missing_monitor_404() {
        let (base, _s) = spawn().await;
        let resp = reqwest::get(format!("{base}/api/v1/monitors/999/history")).await.unwrap();
        assert_eq!(resp.status(), 404);
    }
```

- [ ] **Step 2: Run → fails. Step 3: Implement.**

Add routes in `build_app` (before `.fallback(...)`):
```rust
        .route("/api/v1/monitors/{id}/history", get(monitor_history))
        .route("/api/v1/monitors/{id}/uptime", get(monitor_uptime))
```
Add handlers (import `use crate::store::Sample; use crate::uptime::{compute_uptime, Uptime}; use crate::status::Status; use axum::extract::Query; use std::collections::HashMap; use std::time::{SystemTime, UNIX_EPOCH};`):
```rust
fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn monitor_history(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<Sample>>, StatusCode> {
    if state.store.get_monitor(id).await.map_err(internal)?.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let samples = state.store.get_samples(id, limit).await.map_err(internal)?;
    Ok(Json(samples))
}

async fn monitor_uptime(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Uptime>, StatusCode> {
    if state.store.get_monitor(id).await.map_err(internal)?.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let window = q
        .get("window")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(86_400)
        .clamp(60, 90 * 86_400);
    let now = now_epoch();
    let window_start = now - window;
    let prior = state
        .store
        .status_at(id, window_start)
        .await
        .map_err(internal)?
        .unwrap_or(Status::Unknown);
    let transitions = state
        .store
        .get_transitions_since(id, window_start)
        .await
        .map_err(internal)?;
    Ok(Json(compute_uptime(prior, &transitions, window_start, now)))
}
```

- [ ] **Step 4: tests pass. Commit.**

```bash
cargo test api::
cargo test
cargo +nightly fmt
git add src/api.rs
git commit -m "feat: history and uptime API endpoints"
```

---

### Task 5: UI — uptime timeline + history panel

**Files:** Modify `ui/src/api.ts`, `ui/src/types.ts`, `ui/src/components/MonitorCard.tsx` (or a new `MonitorDetail.tsx` / reuse `Modal`), `ui/src/styles.css`.

**Interfaces:** `ApiClient` gains `getHistory(id, limit?)` and `getUptime(id, windowSecs)`. Types: `Sample { status, message, components, at }` and `Uptime { window_secs, ok_secs, degraded_secs, critical_secs, unknown_secs, percent_ok, segments: {status,start,end}[] }`.

- [ ] **Step 1: Add API client methods + types** matching Task 4's JSON. (`at` is epoch seconds — render via `new Date(at * 1000)`.)

- [ ] **Step 2: Build the detail view.** When a monitor card is opened (expanded or in a modal — match existing UX), fetch `getUptime(id, window)` and `getHistory(id, 100)` and render:
  - an **uptime bar**: horizontal segments colored by status (ok/degraded/critical/unknown), widths proportional to `(end-start)/window_secs`, with a hover title showing the status + local time range; plus the **`percent_ok`** figure and a small window toggle (24h / 7d → 86400 / 604800).
  - a **history panel**: reverse-chronological list of samples — `local time · status dot · message · (component count)` — scroll-back for forensics. Keep it compact; reuse the status colors and dark/light theming.
- Fetch on open and when the window toggle changes; handle empty history gracefully ("no history yet").

- [ ] **Step 3: Build.** `npm --prefix ui run build` — clean tsc + vite.

- [ ] **Step 4: Commit.**
```bash
git add ui
git commit -m "feat: UI uptime timeline + history panel"
```

(Controller verifies in a browser via Playwright against a running backend with recorded history — see Verification.)

---

### Task 6: Wire retention config

**Files:** Modify `src/config.rs`, `src/main.rs`, `README.md`.

- [ ] **Step 1:** In `Config`, add `pub retention_days: i64`, read from `HEALTH_SAMPLE_RETENTION_DAYS` (default `7`) in `resolve`. Add a test for default + override.
- [ ] **Step 2:** In `main`, build the scheduler with `.retention_days(config.retention_days)`.
- [ ] **Step 3:** Note `HEALTH_SAMPLE_RETENTION_DAYS` in the README env list.
- [ ] **Step 4:** `cargo test`, fmt, commit `src/config.rs src/main.rs README.md` with `feat: configurable sample retention (HEALTH_SAMPLE_RETENTION_DAYS, default 7)`.

---

## Verification (controller-run)

- `cargo test` covers store history/prune, `compute_uptime`, scheduler recording, and the API endpoints.
- Browser (Playwright): run the binary with a temp DB; seed a monitor and record a transition + several samples via the API/scheduler; open the monitor's detail in the UI and confirm the uptime bar renders segments + a percent, and the history panel lists samples newest-first. Confirm window toggle (24h/7d) re-fetches.

## What this delivers

Per-monitor uptime timelines (from debounced transitions, kept indefinitely) and a 7-day forensic history of raw check samples — so after a miss you can scroll back through exactly what each check saw and decide what to tune. Numeric metric trend-lines (Layer 3) remain deferred.
