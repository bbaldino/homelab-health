use crate::incident::{
    FailingComponent, Incident, IncidentDetail, RECENT_LIMIT, RECENT_WINDOW_SECS, compute_incidents,
};
use crate::report::{CheckReport, Component};
use crate::status::Status;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall clock as epoch seconds — the unit every `at` column is stored in.
pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

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
    /// Incidents overlapping the last 24h, newest 5, without
    /// `failing_components` — enough to render "green, but 2 incidents
    /// overnight" without a second request, and cheap enough for a polling
    /// dashboard.
    pub recent_incidents: Vec<Incident>,
}

/// One row of `status_transitions`. Uptime only needs `(status, at)` and
/// ignores the message, but incidents need it for their rollup line — one
/// representation of a transition row, not two.
#[derive(Debug, Clone, Serialize)]
pub struct TransitionRow {
    pub status: Status,
    pub message: String,
    pub at: i64,
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

fn row_to_transition(row: SqliteRow) -> TransitionRow {
    let status: String = row.try_get("status").unwrap_or_default();
    TransitionRow {
        status: Status::from_db(&status),
        message: row.try_get("message").unwrap_or_default(),
        at: row.try_get("at").unwrap_or_default(),
    }
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
    ) -> Result<Vec<TransitionRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT status, message, at FROM status_transitions
             WHERE monitor_id = ?1 AND at > ?2 ORDER BY at ASC, id ASC",
        )
        .bind(monitor_id)
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_transition).collect())
    }

    /// The last transition at or before `at` — the monitor's committed status
    /// going into a window, plus when that status actually started. `None`
    /// means nothing was ever recorded at or before `at`: status is unknown by
    /// absence, which incidents and uptime treat differently.
    pub async fn last_transition_at_or_before(
        &self,
        monitor_id: i64,
        at: i64,
    ) -> Result<Option<TransitionRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT status, message, at FROM status_transitions
             WHERE monitor_id = ?1 AND at <= ?2 ORDER BY at DESC, id DESC LIMIT 1",
        )
        .bind(monitor_id)
        .bind(at)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_transition))
    }

    /// Batched `get_transitions_since` across every monitor. The `/status`
    /// polling path is already N+1 on `status_current`; incidents must not add
    /// to it, so both transition reads happen once for the whole board.
    pub async fn transitions_since_all(
        &self,
        since: i64,
    ) -> Result<HashMap<i64, Vec<TransitionRow>>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT monitor_id, status, message, at FROM status_transitions
             WHERE at > ?1 ORDER BY monitor_id ASC, at ASC, id ASC",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        let mut out: HashMap<i64, Vec<TransitionRow>> = HashMap::new();
        for r in rows {
            let monitor_id: i64 = r.try_get("monitor_id").unwrap_or_default();
            out.entry(monitor_id)
                .or_default()
                .push(row_to_transition(r));
        }
        Ok(out)
    }

    /// Batched `last_transition_at_or_before` across every monitor.
    pub async fn last_transition_at_or_before_all(
        &self,
        at: i64,
    ) -> Result<HashMap<i64, TransitionRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT monitor_id, status, message, at FROM (
                 SELECT monitor_id, status, message, at,
                        ROW_NUMBER() OVER (
                            PARTITION BY monitor_id ORDER BY at DESC, id DESC
                        ) AS rn
                 FROM status_transitions WHERE at <= ?1
             ) WHERE rn = 1",
        )
        .bind(at)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let monitor_id: i64 = r.try_get("monitor_id").unwrap_or_default();
                (monitor_id, row_to_transition(r))
            })
            .collect())
    }

    /// The transition that *opened* the incident in force at `at`, if any: the
    /// earliest non-Ok transition following the most recent Ok transition at or
    /// before `at`. `None` when the monitor was Ok going into `at`, or has no
    /// transitions at all — both correctly mean "no incident open".
    ///
    /// This is deliberately not [`Store::last_transition_at_or_before`]. A
    /// monitor can commit several non-Ok transitions in a row with no
    /// intervening Ok — the scheduler's debounce lives in memory, so a restart
    /// re-commits the current status — and the *last* of that run is not when
    /// the outage began. Seeding an incident from it reports `started_at` too
    /// late, and differently depending on the query window, which is exactly
    /// what "an incident is never truncated at a query boundary" forbids.
    pub async fn open_incident_start(
        &self,
        monitor_id: i64,
        at: i64,
    ) -> Result<Option<TransitionRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT status, message, at FROM status_transitions t
             WHERE t.monitor_id = ?1 AND t.at <= ?2 AND t.status != 'ok'
               AND NOT EXISTS (
                   SELECT 1 FROM status_transitions ok
                   WHERE ok.monitor_id = t.monitor_id AND ok.status = 'ok'
                     AND ok.at <= ?2
                     AND (ok.at > t.at OR (ok.at = t.at AND ok.id > t.id))
               )
             ORDER BY t.at ASC, t.id ASC LIMIT 1",
        )
        .bind(monitor_id)
        .bind(at)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_transition))
    }

    /// Batched [`Store::open_incident_start`] across every monitor.
    ///
    /// `ok_after` marks rows that have a later Ok at or before `at` — the window
    /// runs backwards, and the row itself is non-Ok, so it can only be set by a
    /// strictly later recovery. What survives is each monitor's open run; `rn`
    /// then takes the earliest row of it.
    pub async fn open_incident_start_all(
        &self,
        at: i64,
    ) -> Result<HashMap<i64, TransitionRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT monitor_id, status, message, at FROM (
                 SELECT monitor_id, status, message, at,
                        ROW_NUMBER() OVER (
                            PARTITION BY monitor_id ORDER BY at ASC, id ASC
                        ) AS rn
                 FROM (
                     SELECT monitor_id, status, message, at, id,
                            MAX(CASE WHEN status = 'ok' THEN 1 ELSE 0 END) OVER (
                                PARTITION BY monitor_id ORDER BY at DESC, id DESC
                                ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
                            ) AS ok_after
                     FROM status_transitions WHERE at <= ?1
                 ) WHERE status != 'ok' AND ok_after = 0
             ) WHERE rn = 1",
        )
        .bind(at)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let monitor_id: i64 = r.try_get("monitor_id").unwrap_or_default();
                (monitor_id, row_to_transition(r))
            })
            .collect())
    }

    /// Components that were non-Ok at any point in `[start, end]`, folded per
    /// name across every sample in the span. A snapshot from a single sample
    /// would tell half the story: components fail at different moments.
    ///
    /// Samples are pruned after 7 days while transitions are kept forever, so
    /// an old incident legitimately yields `[]` — timing, `worst_status` and
    /// `message` survive without it.
    pub async fn failing_components_for(
        &self,
        monitor_id: i64,
        start: i64,
        end: i64,
    ) -> Result<Vec<FailingComponent>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT components_json, at FROM check_samples
             WHERE monitor_id = ?1 AND at >= ?2 AND at <= ?3 ORDER BY at ASC, id ASC",
        )
        .bind(monitor_id)
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await?;

        let mut folded: HashMap<String, FailingComponent> = HashMap::new();
        for r in rows {
            let at: i64 = r.try_get("at").unwrap_or_default();
            let components_str: String = r.try_get("components_json").unwrap_or_default();
            let components: Vec<Component> =
                serde_json::from_str(&components_str).unwrap_or_default();
            for c in components.into_iter().filter(|c| c.status != Status::Ok) {
                match folded.get_mut(&c.name) {
                    Some(f) => {
                        // Only a strictly worse status takes over the message,
                        // so earlier wins on ties.
                        if c.status.rank() > f.worst_status.rank() {
                            f.worst_status = c.status;
                            f.critical = c.critical;
                            f.message = c.message;
                        }
                        f.last_seen = at;
                    }
                    None => {
                        folded.insert(
                            c.name.clone(),
                            FailingComponent {
                                name: c.name,
                                worst_status: c.status,
                                critical: c.critical,
                                message: c.message,
                                first_seen: at,
                                last_seen: at,
                            },
                        );
                    }
                }
            }
        }

        // Worst first, then by when trouble started — a stable order that puts
        // the component that explains the incident at the top.
        let mut out: Vec<FailingComponent> = folded.into_values().collect();
        out.sort_by(|a, b| {
            b.worst_status
                .rank()
                .cmp(&a.worst_status.rank())
                .then(a.first_seen.cmp(&b.first_seen))
                .then(a.name.cmp(&b.name))
        });
        Ok(out)
    }

    /// Incidents across monitors overlapping `[since, until]`, newest first.
    ///
    /// Transitions are read all the way to now rather than to `until`, so an
    /// incident that recovered after the requested range still reports its real
    /// `ended_at`: incidents overlapping the range are returned whole, because
    /// truncating one at a query boundary would misreport its duration.
    pub async fn list_incidents(
        &self,
        since: i64,
        until: i64,
        monitor_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<IncidentDetail>, sqlx::Error> {
        let monitors = match monitor_id {
            // An unknown id is an empty result, not a 404: it is a filter, not
            // a path segment.
            Some(id) => self.get_monitor(id).await?.into_iter().collect(),
            None => self.list_monitors().await?,
        };
        let now = now_epoch();
        // The transition that *opened* the incident in force at `since`, not
        // merely the last one before it: a run of non-Ok commits with no
        // intervening Ok would otherwise report `started_at` at the run's tail,
        // making the same incident look shorter through a narrower window.
        let mut priors = self.open_incident_start_all(since).await?;
        let mut transitions = self.transitions_since_all(since).await?;

        let mut out = Vec::new();
        for monitor in monitors {
            let rows = transitions.remove(&monitor.id).unwrap_or_default();
            let prior = priors.remove(&monitor.id);
            for incident in compute_incidents(prior.as_ref(), &rows, now) {
                if incident.started_at > until {
                    continue;
                }
                let end = incident.ended_at.unwrap_or(now);
                let failing_components = self
                    .failing_components_for(monitor.id, incident.started_at, end)
                    .await?;
                out.push(IncidentDetail {
                    monitor_id: monitor.id,
                    monitor_name: monitor.name.clone(),
                    incident,
                    failing_components,
                });
            }
        }

        out.sort_by(|a, b| {
            b.incident
                .started_at
                .cmp(&a.incident.started_at)
                .then(a.monitor_id.cmp(&b.monitor_id))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Seed a sample (components and all) at an exact epoch second, so tests can
    /// lay out an incident's timeline without sleeping.
    #[cfg(test)]
    pub(crate) async fn insert_sample_with_components_at(
        &self,
        monitor_id: i64,
        report: &CheckReport,
        at: i64,
    ) {
        let components = serde_json::to_string(&report.components).unwrap();
        sqlx::query(
            "INSERT INTO check_samples (monitor_id, status, message, components_json, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(monitor_id)
        .bind(report.status.as_str())
        .bind(&report.message)
        .bind(components)
        .bind(at)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// Seed a transition at an exact epoch second.
    #[cfg(test)]
    pub(crate) async fn insert_transition_at(
        &self,
        monitor_id: i64,
        status: Status,
        message: &str,
        at: i64,
    ) {
        sqlx::query(
            "INSERT INTO status_transitions (monitor_id, status, message, at)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(monitor_id)
        .bind(status.as_str())
        .bind(message)
        .bind(at)
        .execute(&self.pool)
        .await
        .unwrap();
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
        let mut status = build_status(monitor, row);
        let now = now_epoch();
        let window_start = now - RECENT_WINDOW_SECS;
        let prior = self.open_incident_start(id, window_start).await?;
        let transitions = self.get_transitions_since(id, window_start).await?;
        status.recent_incidents = compute_incidents(prior.as_ref(), &transitions, now);
        status.recent_incidents.truncate(RECENT_LIMIT);
        Ok(Some(status))
    }

    pub async fn list_status(&self) -> Result<Vec<MonitorStatus>, sqlx::Error> {
        let monitors = self.list_monitors().await?;
        let now = now_epoch();
        let window_start = now - RECENT_WINDOW_SECS;
        // Two batched reads for the whole board, not two per monitor: this is
        // the path a dashboard polls every 60s.
        let mut priors = self.open_incident_start_all(window_start).await?;
        let mut transitions = self.transitions_since_all(window_start).await?;

        let mut out = Vec::with_capacity(monitors.len());
        for monitor in monitors {
            let rows = transitions.remove(&monitor.id).unwrap_or_default();
            let prior = priors.remove(&monitor.id);
            let row = sqlx::query(
                "SELECT status, message, components_json, updated_at
                 FROM status_current WHERE monitor_id = ?1",
            )
            .bind(monitor.id)
            .fetch_optional(&self.pool)
            .await?;
            let mut status = build_status(monitor, row);
            status.recent_incidents = compute_incidents(prior.as_ref(), &rows, now);
            status.recent_incidents.truncate(RECENT_LIMIT);
            out.push(status);
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
            recent_incidents: Vec::new(),
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
                recent_incidents: Vec::new(),
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
        assert_eq!(since[0].status, Status::Ok); // ascending
        assert_eq!(since[0].message, "up");
        // "now+large" should find the latest transition (Critical)
        let at_now = s
            .last_transition_at_or_before(m.id, 9_999_999_999)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(at_now.status, Status::Critical);
        assert_eq!(at_now.message, "down");
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

    fn report_with(components: Vec<Component>) -> CheckReport {
        CheckReport::from_components(components)
    }

    #[tokio::test]
    async fn failing_components_folds_across_samples() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        // disk3 is down from the start; cache free space only blips later, and
        // escalates while it is failing. An ok component is never reported.
        s.insert_sample_with_components_at(
            m.id,
            &report_with(vec![
                Component::new("disk3", Status::Critical, true, "SMART: FAILING_NOW"),
                Component::new("cache free space", Status::Ok, false, ""),
            ]),
            100,
        )
        .await;
        s.insert_sample_with_components_at(
            m.id,
            &report_with(vec![
                Component::new("disk3", Status::Critical, true, "SMART: FAILING_NOW"),
                Component::new("cache free space", Status::Degraded, false, "9% free"),
            ]),
            200,
        )
        .await;
        s.insert_sample_with_components_at(
            m.id,
            &report_with(vec![
                Component::new("disk3", Status::Ok, true, ""),
                Component::new("cache free space", Status::Degraded, false, "4% free"),
            ]),
            300,
        )
        .await;

        let fc = s.failing_components_for(m.id, 100, 300).await.unwrap();
        assert_eq!(fc.len(), 2);
        // worst first
        assert_eq!(fc[0].name, "disk3");
        assert_eq!(fc[0].worst_status, Status::Critical);
        assert!(fc[0].critical);
        assert_eq!(fc[0].message, "SMART: FAILING_NOW");
        assert_eq!(fc[0].first_seen, 100);
        // disk3 recovered at 300, so it was last seen failing at 200.
        assert_eq!(fc[0].last_seen, 200);

        assert_eq!(fc[1].name, "cache free space");
        assert_eq!(fc[1].worst_status, Status::Degraded);
        assert!(!fc[1].critical);
        // Degraded twice: earlier message wins on a severity tie.
        assert_eq!(fc[1].message, "9% free");
        assert_eq!(fc[1].first_seen, 200);
        assert_eq!(fc[1].last_seen, 300);
    }

    #[tokio::test]
    async fn failing_components_escalation_takes_the_worse_message() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        s.insert_sample_with_components_at(
            m.id,
            &report_with(vec![Component::new("db", Status::Degraded, true, "slow")]),
            100,
        )
        .await;
        s.insert_sample_with_components_at(
            m.id,
            &report_with(vec![Component::new("db", Status::Critical, true, "gone")]),
            200,
        )
        .await;
        let fc = s.failing_components_for(m.id, 100, 200).await.unwrap();
        assert_eq!(fc.len(), 1);
        assert_eq!(fc[0].worst_status, Status::Critical);
        assert_eq!(fc[0].message, "gone");
        assert_eq!(fc[0].first_seen, 100);
        assert_eq!(fc[0].last_seen, 200);
    }

    #[tokio::test]
    async fn failing_components_is_empty_when_samples_are_pruned() {
        // Samples live 7 days, transitions forever. An old incident degrades
        // gracefully to no component detail rather than erroring.
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        let fc = s.failing_components_for(m.id, 100, 300).await.unwrap();
        assert!(fc.is_empty());
    }

    #[tokio::test]
    async fn list_incidents_attaches_monitor_and_components() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        let now = now_epoch();
        s.insert_transition_at(m.id, Status::Ok, "up", now - 7200)
            .await;
        s.insert_transition_at(
            m.id,
            Status::Critical,
            "2 of 8 components unhealthy",
            now - 3600,
        )
        .await;
        s.insert_transition_at(m.id, Status::Ok, "recovered", now - 1800)
            .await;
        s.insert_sample_with_components_at(
            m.id,
            &report_with(vec![Component::new(
                "disk3",
                Status::Critical,
                true,
                "SMART",
            )]),
            now - 3000,
        )
        .await;

        let incidents = s.list_incidents(now - 86_400, now, None, 50).await.unwrap();
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].monitor_id, m.id);
        assert_eq!(incidents[0].monitor_name, "Plex");
        assert_eq!(incidents[0].incident.duration_secs, 1800);
        assert_eq!(incidents[0].failing_components.len(), 1);
        assert_eq!(incidents[0].failing_components[0].name, "disk3");
    }

    #[tokio::test]
    async fn list_incidents_returns_overlapping_incidents_whole() {
        // The outage started before `since` and ended after `until`; it must be
        // reported with its real boundaries, not truncated to the query range.
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        let now = now_epoch();
        s.insert_transition_at(m.id, Status::Ok, "up", now - 10_000)
            .await;
        s.insert_transition_at(m.id, Status::Critical, "down", now - 8000)
            .await;
        s.insert_transition_at(m.id, Status::Ok, "recovered", now - 2000)
            .await;

        let incidents = s
            .list_incidents(now - 6000, now - 4000, None, 50)
            .await
            .unwrap();
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].incident.started_at, now - 8000);
        assert_eq!(incidents[0].incident.ended_at, Some(now - 2000));
    }

    #[tokio::test]
    async fn open_incident_start_returns_the_earliest_of_a_non_ok_run() {
        // Consecutive non-Ok commits with no intervening Ok are not
        // theoretical: the scheduler's debounce lives in memory, so a restart
        // re-commits the status the monitor is already in.
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        s.insert_transition_at(m.id, Status::Critical, "old outage", 100)
            .await;
        s.insert_transition_at(m.id, Status::Ok, "recovered", 1_000)
            .await;
        s.insert_transition_at(m.id, Status::Degraded, "disk3 failing", 2_000)
            .await;
        s.insert_transition_at(m.id, Status::Degraded, "disk3 failing", 3_000)
            .await;
        s.insert_transition_at(m.id, Status::Degraded, "disk3 failing", 4_000)
            .await;

        // The run opened at 2_000, not at the 4_000 re-commit and not at the
        // already-closed outage at 100.
        let open = s.open_incident_start(m.id, 5_000).await.unwrap().unwrap();
        assert_eq!(open.at, 2_000);
        assert_eq!(open.status, Status::Degraded);
        assert_eq!(
            s.open_incident_start_all(5_000)
                .await
                .unwrap()
                .get(&m.id)
                .unwrap()
                .at,
            2_000
        );
        // Uptime's view is unchanged: it wants the status in force at 5_000,
        // which really is the 4_000 row.
        assert_eq!(
            s.last_transition_at_or_before(m.id, 5_000)
                .await
                .unwrap()
                .unwrap()
                .at,
            4_000
        );
    }

    #[tokio::test]
    async fn open_incident_start_is_none_when_ok_or_absent() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        // No transitions at all: nothing is open, and no incident is faked.
        assert!(s.open_incident_start(m.id, 5_000).await.unwrap().is_none());
        assert!(s.open_incident_start_all(5_000).await.unwrap().is_empty());

        s.insert_transition_at(m.id, Status::Critical, "down", 1_000)
            .await;
        s.insert_transition_at(m.id, Status::Ok, "recovered", 2_000)
            .await;
        assert!(s.open_incident_start(m.id, 3_000).await.unwrap().is_none());
        assert!(s.open_incident_start_all(3_000).await.unwrap().is_empty());
        // The monitor's actual status at 3_000 is still Ok — the query uptime
        // relies on must keep saying so.
        assert_eq!(
            s.last_transition_at_or_before(m.id, 3_000)
                .await
                .unwrap()
                .unwrap()
                .status,
            Status::Ok
        );
    }

    #[tokio::test]
    async fn the_same_incident_reports_identical_timing_through_any_window() {
        // One long outage, re-committed on restart, then recovered. Asking for
        // 1 day and asking for 7 days must describe the same event the same
        // way: an incident is never truncated at a query boundary.
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        let now = now_epoch();
        s.insert_transition_at(m.id, Status::Ok, "up", now - 500_000)
            .await;
        s.insert_transition_at(m.id, Status::Degraded, "disk3 failing", now - 400_000)
            .await;
        // A restart re-commits the same status *between* the outage's start and
        // the 1-day window start. Seeding from the last transition before the
        // window would make this row look like the beginning of the outage.
        s.insert_transition_at(m.id, Status::Degraded, "disk3 failing", now - 200_000)
            .await;
        s.insert_transition_at(m.id, Status::Degraded, "disk3 failing", now - 30_000)
            .await;
        s.insert_transition_at(m.id, Status::Ok, "recovered", now - 10_000)
            .await;

        let day = s.list_incidents(now - 86_400, now, None, 50).await.unwrap();
        let week = s
            .list_incidents(now - 7 * 86_400, now, None, 50)
            .await
            .unwrap();
        assert_eq!(day.len(), 1);
        assert_eq!(week.len(), 1);
        assert_eq!(day[0].incident, week[0].incident);
        assert_eq!(day[0].incident.started_at, now - 400_000);
        assert_eq!(day[0].incident.duration_secs, 390_000);

        // The inline `/status` path reads the same event through its own fixed
        // 24h window and must not disagree either.
        let inline = s.list_status().await.unwrap();
        assert_eq!(inline[0].recent_incidents[0], day[0].incident);
    }

    #[tokio::test]
    async fn list_status_includes_bounded_recent_incidents() {
        let s = store().await;
        let m = s.create_monitor(sample()).await.unwrap();
        let now = now_epoch();
        s.insert_transition_at(m.id, Status::Ok, "up", now - 80_000)
            .await;
        // Six flaps inside the window; only the five newest are inlined.
        for i in 0..6 {
            let at = now - 70_000 + i * 1000;
            s.insert_transition_at(m.id, Status::Critical, "down", at)
                .await;
            s.insert_transition_at(m.id, Status::Ok, "up", at + 100)
                .await;
        }
        let all = s.list_status().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].recent_incidents.len(), RECENT_LIMIT);
        // newest first
        assert_eq!(all[0].recent_incidents[0].started_at, now - 65_000);
    }

    #[tokio::test]
    async fn brand_new_monitor_has_no_recent_incidents() {
        let s = store().await;
        s.create_monitor(sample()).await.unwrap();
        let all = s.list_status().await.unwrap();
        assert!(all[0].recent_incidents.is_empty());
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
