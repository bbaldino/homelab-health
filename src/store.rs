use crate::report::{CheckReport, Component};
use crate::status::Status;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub type_id: String,
    pub config: Value,
    pub interval_secs: i64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorStatus {
    #[serde(flatten)]
    pub monitor: Monitor,
    pub status: Option<Status>,
    pub message: Option<String>,
    pub components: Vec<Component>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub status: Status,
    pub message: String,
    pub components: Vec<Component>,
    pub at: i64,
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

fn row_to_monitor(row: SqliteRow) -> Result<Monitor, sqlx::Error> {
    let config_str: String = row.try_get("config_json")?;
    Ok(Monitor {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        type_id: row.try_get("type_id")?,
        config: serde_json::from_str(&config_str).unwrap_or(Value::Null),
        interval_secs: row.try_get("interval_secs")?,
        enabled: row.try_get::<i64, _>("enabled")? != 0,
    })
}

impl Store {
    pub async fn connect(url: &str) -> Result<Store, sqlx::Error> {
        // create_if_missing so a fresh file-backed DB is bootstrapped on first run;
        // harmless for sqlite::memory:. In-memory DBs stay at 1 connection so tests
        // that depend on single connection behaviour still work.
        let is_memory = url.contains(":memory:") || url.contains("mode=memory");
        let max_conns = if is_memory { 1 } else { 5 };
        let mut options = SqliteConnectOptions::from_str(url)?.create_if_missing(true);
        if !is_memory {
            options = options
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(5));
        }
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conns)
            .connect_with(options)
            .await?;
        // raw_sql (not query) so BOTH CREATE TABLE statements run.
        sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
            .execute(&pool)
            .await?;
        sqlx::raw_sql(include_str!("../migrations/0002_history.sql"))
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
        .try_get("id")?;

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
        rows.into_iter().map(row_to_monitor).collect()
    }

    pub async fn get_monitor(&self, id: i64) -> Result<Option<Monitor>, sqlx::Error> {
        let row = sqlx::query("SELECT * FROM monitors WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_monitor).transpose()
    }

    pub async fn save_status(
        &self,
        monitor_id: i64,
        report: &CheckReport,
    ) -> Result<(), sqlx::Error> {
        let status = serde_json::to_string(&report.status).unwrap_or_default();
        let components = serde_json::to_string(&report.components).unwrap_or_default();
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
                let status_str: String = r.try_get("status")?;
                let message: String = r.try_get("message")?;
                let status: Status =
                    serde_json::from_value(Value::String(status_str)).unwrap_or(Status::Unknown);
                Ok(Some((status, message)))
            }
            None => Ok(None),
        }
    }

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
        let retention_days = retention_days.max(1);
        let res =
            sqlx::query("DELETE FROM check_samples WHERE at < strftime('%s','now') - ?1 * 86400")
                .bind(retention_days)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected())
    }

    pub async fn get_samples(
        &self,
        monitor_id: i64,
        limit: i64,
    ) -> Result<Vec<Sample>, sqlx::Error> {
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

    #[cfg(test)]
    async fn insert_sample_at(
        &self,
        monitor_id: i64,
        status: Status,
        message: &str,
        days_ago: i64,
    ) {
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
}

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

    #[tokio::test]
    async fn connects_and_creates_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("health.db");
        let url = format!("sqlite://{}", path.display());
        // File does not exist yet; connect must create it.
        let s = Store::connect(&url).await.unwrap();
        let m = s.create_monitor(sample()).await.unwrap();
        assert!(m.id > 0);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn connect_is_idempotent_on_existing_file() {
        // Reconnecting to a file whose tables already exist (e.g. restarting the
        // daemon) must not fail — the migration is guarded with IF NOT EXISTS.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("health.db");
        let url = format!("sqlite://{}", path.display());
        let s1 = Store::connect(&url).await.unwrap();
        s1.create_monitor(sample()).await.unwrap();
        drop(s1);
        // Second connect against the now-populated file must succeed and see the row.
        let s2 = Store::connect(&url).await.unwrap();
        assert_eq!(s2.list_monitors().await.unwrap().len(), 1);
    }

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
        report.components.push(crate::report::Component::new(
            "db",
            crate::status::Status::Critical,
            true,
            "down",
        ));
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

    #[tokio::test]
    async fn records_and_reads_samples() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        s.record_sample(m.id, &CheckReport::new(Status::Critical, "boom"))
            .await
            .unwrap();
        s.record_sample(m.id, &CheckReport::ok("fine"))
            .await
            .unwrap();
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
        s.record_transition(m.id, Status::Critical, "down")
            .await
            .unwrap();
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
        s.record_sample(m.id, &CheckReport::ok("new"))
            .await
            .unwrap();
        let deleted = s.prune_samples(7).await.unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(s.get_samples(m.id, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deleting_monitor_cascades_history() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        s.record_sample(m.id, &CheckReport::ok("x")).await.unwrap();
        s.record_transition(m.id, Status::Ok, "up").await.unwrap();
        assert!(s.delete_monitor(m.id).await.unwrap());
        assert!(s.get_samples(m.id, 10).await.unwrap().is_empty());
        assert!(s.get_transitions_since(m.id, 0).await.unwrap().is_empty());
    }
}
