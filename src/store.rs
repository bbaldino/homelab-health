use crate::report::CheckReport;
use crate::status::Status;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

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
        // harmless for sqlite::memory:. max_connections(1) keeps an in-memory DB
        // alive for the whole test.
        let options = SqliteConnectOptions::from_str(url)?.create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        // raw_sql (not query) so BOTH CREATE TABLE statements run.
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
                let status_str: String = r.try_get("status")?;
                let message: String = r.try_get("message")?;
                let status: Status =
                    serde_json::from_value(Value::String(status_str)).unwrap_or(Status::Unknown);
                Ok(Some((status, message)))
            }
            None => Ok(None),
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
}
