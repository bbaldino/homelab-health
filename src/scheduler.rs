use crate::check::Registry;
use crate::report::CheckReport;
use crate::status::Status;
use crate::store::{Monitor, Store};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Commits a status change only after `threshold` consecutive matching
/// observations, so a transient blip never flips the committed status.
pub struct Debounce {
    threshold: u32,
    committed: Option<Status>,
    candidate: Option<Status>,
    count: u32,
}

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
    pub async fn run_and_record(&mut self, monitor: &Monitor) -> Result<CheckReport, sqlx::Error> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewMonitor;
    use serde_json::json;

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
        let (store, m) = store_with_monitor(
            "tcp",
            json!({ "host": "127.0.0.1", "port": port, "timeout_secs": 1 }),
        )
        .await;
        let mut sched = Scheduler::new(store.clone(), Arc::new(Registry::with_builtins()), 2);

        // First observation: not yet committed, nothing persisted.
        sched.run_and_record(&m).await.unwrap();
        assert!(
            store
                .get_status(m.id)
                .await
                .unwrap()
                .unwrap()
                .status
                .is_none()
        );

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
}
