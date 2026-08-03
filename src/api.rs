use crate::check::{ConfigSchema, Registry};
use crate::incident::IncidentDetail;
use crate::status::Status;
use crate::store::{Monitor, MonitorStatus, NewMonitor, Sample, Store, now_epoch};
use crate::uptime::{Uptime, compute_uptime};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct ApiState {
    pub store: Store,
    pub registry: Arc<Registry>,
}

/// Handler error: a status plus a plain-text body. axum renders the tuple as
/// `text/plain`, so a rejected request explains itself instead of arriving as a
/// bare status code.
type ApiError = (StatusCode, String);

fn internal(e: sqlx::Error) -> ApiError {
    tracing::error!("db error: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal error".to_string(),
    )
}

fn not_found() -> ApiError {
    (StatusCode::NOT_FOUND, "not found".to_string())
}

/// Timestamps above this are rejected as implausible epoch seconds. The value
/// consumers actually send by mistake is `Date.now()` in milliseconds, which
/// parses as a perfectly good integer somewhere around the year 5138 and would
/// otherwise return an empty window with a 200 — indistinguishable from
/// "nothing happened".
const MAX_EPOCH_SECS: i64 = 4_102_444_800; // 2100-01-01T00:00:00Z

/// Parses an optional integer query parameter.
///
/// Absent yields `Ok(None)` and the caller applies its default. Present but
/// unparseable (or outside `valid`) is a 400 naming the parameter — never a
/// silent fallback to the default, which would hand a consumer a successful
/// response to a request the server did not actually honor.
///
/// Range-checking here is for values that are malformed as their *type*.
/// Parameters with a meaningful range (`limit`, `window`) pass `FULL` and clamp
/// afterwards: clamping a valid number is a kindness, accepting a malformed one
/// is not.
fn int_param(
    q: &HashMap<String, String>,
    name: &str,
    expected: &str,
    valid: std::ops::RangeInclusive<i64>,
) -> Result<Option<i64>, ApiError> {
    let Some(raw) = q.get(name) else {
        return Ok(None);
    };
    match raw.parse::<i64>() {
        Ok(v) if valid.contains(&v) => Ok(Some(v)),
        _ => Err((
            StatusCode::BAD_REQUEST,
            format!("invalid \"{name}\": expected {expected}, got \"{raw}\""),
        )),
    }
}

const FULL: std::ops::RangeInclusive<i64> = i64::MIN..=i64::MAX;

/// An epoch-seconds parameter, defaulted when absent.
fn epoch_param(q: &HashMap<String, String>, name: &str, default: i64) -> Result<i64, ApiError> {
    let expected = format!("epoch seconds (integer 0..{MAX_EPOCH_SECS})");
    Ok(int_param(q, name, &expected, 0..=MAX_EPOCH_SECS)?.unwrap_or(default))
}

pub fn build_app(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/version", get(version))
        .route("/api/v1/check-types", get(check_types))
        .route(
            "/api/v1/checks/prometheus/inspect",
            post(prometheus_inspect),
        )
        .route("/api/v1/monitors", get(list_monitors).post(create_monitor))
        .route(
            "/api/v1/monitors/{id}",
            axum::routing::put(update_monitor).delete(delete_monitor),
        )
        .route("/api/v1/status", get(list_status))
        .route("/api/v1/status/{id}", get(get_status))
        .route("/api/v1/monitors/{id}/run", post(run_now))
        .route("/api/v1/monitors/{id}/history", get(monitor_history))
        .route("/api/v1/monitors/{id}/uptime", get(monitor_uptime))
        .route("/api/v1/incidents", get(list_incidents))
        .fallback(crate::ui::serve_asset)
        .with_state(state)
}

/// Reports the running build's version, read from the crate version at compile
/// time. The release tag drives Cargo.toml, so this reflects the released image.
async fn version() -> Json<Value> {
    Json(json!({ "version": env!("CARGO_PKG_VERSION") }))
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

#[derive(serde::Deserialize)]
struct InspectRequest {
    url: String,
    timeout_secs: Option<u64>,
}

async fn prometheus_inspect(
    Json(req): Json<InspectRequest>,
) -> Result<Json<crate::check::prometheus::InspectResult>, ApiError> {
    let timeout = req.timeout_secs.unwrap_or(10);
    match crate::check::prometheus::fetch_and_parse(&req.url, timeout).await {
        Ok(series) => Ok(Json(crate::check::prometheus::inspect_result(&series))),
        Err(e) => {
            let msg = match e {
                crate::check::prometheus::ScrapeError::Timeout => "timed out".to_string(),
                crate::check::prometheus::ScrapeError::Unreachable(m) => {
                    format!("unreachable: {m}")
                }
                crate::check::prometheus::ScrapeError::BadStatus(c) => format!("HTTP {c}"),
                crate::check::prometheus::ScrapeError::Unparseable(m) => {
                    format!("not Prometheus metrics: {m}")
                }
            };
            Err((StatusCode::BAD_GATEWAY, msg))
        }
    }
}

async fn list_monitors(State(state): State<ApiState>) -> Result<Json<Vec<Monitor>>, ApiError> {
    let monitors = state.store.list_monitors().await.map_err(internal)?;
    Ok(Json(monitors))
}

async fn create_monitor(
    State(state): State<ApiState>,
    Json(body): Json<NewMonitor>,
) -> Result<(StatusCode, Json<Monitor>), ApiError> {
    let monitor = state.store.create_monitor(body).await.map_err(internal)?;
    Ok((StatusCode::CREATED, Json(monitor)))
}

async fn update_monitor(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
    Json(body): Json<NewMonitor>,
) -> Result<Json<Monitor>, ApiError> {
    match state
        .store
        .update_monitor(id, body)
        .await
        .map_err(internal)?
    {
        Some(m) => Ok(Json(m)),
        None => Err(not_found()),
    }
}

async fn delete_monitor(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    if state.store.delete_monitor(id).await.map_err(internal)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found())
    }
}

async fn list_status(State(state): State<ApiState>) -> Result<Json<Vec<MonitorStatus>>, ApiError> {
    let all = state.store.list_status().await.map_err(internal)?;
    Ok(Json(all))
}

async fn get_status(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<MonitorStatus>, ApiError> {
    match state.store.get_status(id).await.map_err(internal)? {
        Some(ms) => Ok(Json(ms)),
        None => Err(not_found()),
    }
}

async fn run_now(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<crate::report::CheckReport>, ApiError> {
    let monitor = match state.store.get_monitor(id).await.map_err(internal)? {
        Some(m) => m,
        None => return Err(not_found()),
    };
    // Run-now persists immediately and intentionally bypasses the scheduler's
    // debounce, so a one-off /run result may momentarily differ from scheduled state.
    let report = state.registry.run(&monitor.type_id, &monitor.config).await;
    state
        .store
        .save_status(id, &report)
        .await
        .map_err(internal)?;
    Ok(Json(report))
}

async fn monitor_history(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<Sample>>, ApiError> {
    // Parameters are validated before the monitor lookup: a malformed request
    // is rejected without touching the database.
    let limit = int_param(&q, "limit", "integer", FULL)?
        .unwrap_or(100)
        .clamp(1, 500);
    if state
        .store
        .get_monitor(id)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err(not_found());
    }
    let samples = state.store.get_samples(id, limit).await.map_err(internal)?;
    Ok(Json(samples))
}

async fn monitor_uptime(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Uptime>, ApiError> {
    let window = int_param(&q, "window", "seconds (integer)", FULL)?
        .unwrap_or(86_400)
        .clamp(60, 90 * 86_400);
    if state
        .store
        .get_monitor(id)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err(not_found());
    }
    let now = now_epoch();
    let window_start = now - window;
    // Uptime only cares which status was in force; it ignores the transition's
    // message, and counts an absent prior as Unknown time rather than dropping it.
    let prior = state
        .store
        .last_transition_at_or_before(id, window_start)
        .await
        .map_err(internal)?
        .map(|t| t.status)
        .unwrap_or(Status::Unknown);
    let transitions: Vec<(Status, i64)> = state
        .store
        .get_transitions_since(id, window_start)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|t| (t.status, t.at))
        .collect();
    Ok(Json(compute_uptime(prior, &transitions, window_start, now)))
}

/// Cross-monitor incident history, newest first, with per-component detail.
///
/// `monitor_id` is a filter rather than a path segment, so an unknown id yields
/// an empty list rather than a 404.
async fn list_incidents(
    State(state): State<ApiState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<IncidentDetail>>, ApiError> {
    let now = now_epoch();
    // Unknown query parameters (`?days=7`) are ignored on purpose, not
    // rejected: that is standard HTTP behavior and what lets a newer consumer
    // talk to an older server.
    let since = epoch_param(&q, "since", now - 7 * 86_400)?;
    let until = epoch_param(&q, "until", now)?;
    let monitor_id = int_param(&q, "monitor_id", "integer", FULL)?;
    let limit = int_param(&q, "limit", "integer", FULL)?
        .unwrap_or(50)
        .clamp(1, 500);
    if since > until {
        // Otherwise an inverted range returns `[]`, the same ambiguity as a
        // silently-defaulted parameter: nothing happened, or you asked wrong?
        return Err((
            StatusCode::BAD_REQUEST,
            format!("invalid range: \"since\" ({since}) is after \"until\" ({until})"),
        ));
    }
    let incidents = state
        .store
        .list_incidents(since, until, monitor_id, limit as usize)
        .await
        .map_err(internal)?;
    Ok(Json(incidents))
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
        assert_eq!(arr.len(), 7);
    }

    #[tokio::test]
    async fn version_endpoint_reports_crate_version() {
        let (base, _store) = spawn().await;
        let body: Value = reqwest::get(format!("{base}/api/v1/version"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
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

    #[tokio::test]
    async fn history_endpoint_returns_samples() {
        let (base, store) = spawn().await;
        let m = store
            .create_monitor(NewMonitor {
                name: "m".into(),
                type_id: "http".into(),
                config: json!({ "url": "http://x" }),
                interval_secs: 30,
                enabled: true,
            })
            .await
            .unwrap();
        store
            .record_sample(m.id, &crate::report::CheckReport::ok("hi"))
            .await
            .unwrap();
        let body: Value = reqwest::get(format!("{base}/api/v1/monitors/{}/history", m.id))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn uptime_endpoint_computes_percent() {
        let (base, store) = spawn().await;
        let m = store
            .create_monitor(NewMonitor {
                name: "m".into(),
                type_id: "http".into(),
                config: json!({ "url": "http://x" }),
                interval_secs: 30,
                enabled: true,
            })
            .await
            .unwrap();
        store
            .record_transition(m.id, crate::status::Status::Ok, "up")
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
        let body: Value = reqwest::get(format!(
            "{base}/api/v1/monitors/{}/uptime?window=3600",
            m.id
        ))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert!(body["percent_ok"].as_f64().unwrap() > 0.0);
    }

    #[tokio::test]
    async fn history_missing_monitor_404() {
        let (base, _s) = spawn().await;
        let resp = reqwest::get(format!("{base}/api/v1/monitors/999/history"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    async fn monitor_named(store: &Store, name: &str) -> Monitor {
        store
            .create_monitor(NewMonitor {
                name: name.into(),
                type_id: "http".into(),
                config: json!({ "url": "http://x" }),
                interval_secs: 30,
                enabled: true,
            })
            .await
            .unwrap()
    }

    /// One closed incident ending `ago` seconds back, lasting 600s.
    async fn seed_incident(store: &Store, id: i64, ago: i64) {
        store
            .insert_transition_at(
                id,
                crate::status::Status::Critical,
                "down",
                now_epoch() - ago,
            )
            .await;
        store
            .insert_transition_at(
                id,
                crate::status::Status::Ok,
                "recovered",
                now_epoch() - ago + 600,
            )
            .await;
    }

    #[tokio::test]
    async fn status_inlines_recent_incidents_without_component_detail() {
        let (base, store) = spawn().await;
        let m = monitor_named(&store, "unraid").await;
        store
            .insert_transition_at(m.id, crate::status::Status::Ok, "up", now_epoch() - 80_000)
            .await;
        // Six incidents inside the 24h window; the inline list is capped at 5.
        for i in 0..6 {
            seed_incident(&store, m.id, 70_000 - i * 1000).await;
        }

        let body: Value = reqwest::get(format!("{base}/api/v1/status"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let incidents = body[0]["recent_incidents"].as_array().unwrap();
        assert_eq!(incidents.len(), 5);
        assert_eq!(incidents[0]["duration_secs"], 600);
        assert_eq!(incidents[0]["worst_status"], "critical");
        assert!(incidents[0]["ended_at"].is_i64());
        // Component detail is deliberately absent from the polling path.
        assert!(incidents[0]["failing_components"].is_null());
    }

    #[tokio::test]
    async fn status_incidents_are_empty_for_a_monitor_that_never_ran() {
        let (base, store) = spawn().await;
        monitor_named(&store, "fresh").await;
        let body: Value = reqwest::get(format!("{base}/api/v1/status"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(body[0]["recent_incidents"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn incidents_endpoint_returns_detail_across_monitors() {
        let (base, store) = spawn().await;
        let a = monitor_named(&store, "unraid").await;
        let b = monitor_named(&store, "plex").await;
        seed_incident(&store, a.id, 3600).await;
        seed_incident(&store, b.id, 7200).await;
        store
            .insert_sample_with_components_at(
                a.id,
                &crate::report::CheckReport::from_components(vec![crate::report::Component::new(
                    "disk3",
                    crate::status::Status::Critical,
                    true,
                    "SMART: FAILING_NOW",
                )]),
                now_epoch() - 3300,
            )
            .await;

        let body: Value = reqwest::get(format!("{base}/api/v1/incidents"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let arr = body.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // newest first
        assert_eq!(arr[0]["monitor_id"], a.id);
        assert_eq!(arr[0]["monitor_name"], "unraid");
        assert_eq!(arr[0]["failing_components"][0]["name"], "disk3");
        assert_eq!(arr[0]["failing_components"][0]["worst_status"], "critical");
        assert_eq!(arr[0]["failing_components"][0]["critical"], true);
        // A monitor with no samples in range still reports the incident.
        assert_eq!(arr[1]["monitor_name"], "plex");
        assert!(arr[1]["failing_components"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn incidents_filter_by_monitor_id_and_since() {
        let (base, store) = spawn().await;
        let a = monitor_named(&store, "a").await;
        let b = monitor_named(&store, "b").await;
        seed_incident(&store, a.id, 3600).await;
        seed_incident(&store, a.id, 400_000).await;
        seed_incident(&store, b.id, 3600).await;

        let by_monitor: Value = reqwest::get(format!(
            "{base}/api/v1/incidents?monitor_id={}&since=0",
            a.id
        ))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(by_monitor.as_array().unwrap().len(), 2);

        // `since` inside the last hour drops the older incident for that monitor.
        let recent: Value = reqwest::get(format!(
            "{base}/api/v1/incidents?monitor_id={}&since={}",
            a.id,
            now_epoch() - 7200
        ))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(recent.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn incidents_clamp_limit() {
        let (base, store) = spawn().await;
        let m = monitor_named(&store, "a").await;
        for i in 0..3 {
            seed_incident(&store, m.id, 3600 + i * 1000).await;
        }

        let limited: Value = reqwest::get(format!("{base}/api/v1/incidents?limit=2"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(limited.as_array().unwrap().len(), 2);

        // Below the floor clamps up to 1 rather than returning nothing.
        let zero: Value = reqwest::get(format!("{base}/api/v1/incidents?limit=0"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(zero.as_array().unwrap().len(), 1);

        // Above the ceiling clamps down without erroring.
        let huge: Value = reqwest::get(format!("{base}/api/v1/incidents?limit=100000"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(huge.as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn incidents_unknown_monitor_id_is_empty_not_404() {
        let (base, _store) = spawn().await;
        let resp = reqwest::get(format!("{base}/api/v1/incidents?monitor_id=999"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.unwrap();
        assert!(body.as_array().unwrap().is_empty());
    }

    /// A malformed parameter must produce a 400 whose body names the parameter,
    /// so a consumer can fix the call from the response alone.
    async fn assert_rejects(url: String, param: &str) {
        let resp = reqwest::get(url).await.unwrap();
        assert_eq!(resp.status(), 400, "expected 400 for {param}");
        let body = resp.text().await.unwrap();
        assert!(
            body.contains(&format!("\"{param}\"")),
            "body {body:?} does not name {param}"
        );
    }

    #[tokio::test]
    async fn history_limit_rejects_unparseable() {
        let (base, store) = spawn().await;
        let m = monitor_named(&store, "a").await;
        assert_rejects(
            format!("{base}/api/v1/monitors/{}/history?limit=abc", m.id),
            "limit",
        )
        .await;
    }

    #[tokio::test]
    async fn history_limit_defaults_and_clamps() {
        let (base, store) = spawn().await;
        let m = monitor_named(&store, "a").await;
        for _ in 0..105 {
            store
                .record_sample(m.id, &crate::report::CheckReport::ok("hi"))
                .await
                .unwrap();
        }

        let default: Value = reqwest::get(format!("{base}/api/v1/monitors/{}/history", m.id))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(default.as_array().unwrap().len(), 100);

        // Out of range clamps rather than 400s: 0 -> 1, huge -> 500 (all 105).
        let floor: Value = reqwest::get(format!("{base}/api/v1/monitors/{}/history?limit=0", m.id))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(floor.as_array().unwrap().len(), 1);

        let ceiling: Value = reqwest::get(format!(
            "{base}/api/v1/monitors/{}/history?limit=100000",
            m.id
        ))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(ceiling.as_array().unwrap().len(), 105);
    }

    #[tokio::test]
    async fn uptime_window_rejects_unparseable() {
        let (base, store) = spawn().await;
        let m = monitor_named(&store, "a").await;
        assert_rejects(
            format!("{base}/api/v1/monitors/{}/uptime?window=abc", m.id),
            "window",
        )
        .await;
    }

    #[tokio::test]
    async fn uptime_window_defaults_and_clamps() {
        let (base, store) = spawn().await;
        let m = monitor_named(&store, "a").await;

        let default: Value = reqwest::get(format!("{base}/api/v1/monitors/{}/uptime", m.id))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(default["window_secs"], 86_400);

        let floor: Value = reqwest::get(format!("{base}/api/v1/monitors/{}/uptime?window=1", m.id))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(floor["window_secs"], 60);

        let ceiling: Value = reqwest::get(format!(
            "{base}/api/v1/monitors/{}/uptime?window=99999999",
            m.id
        ))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(ceiling["window_secs"], 90 * 86_400);
    }

    #[tokio::test]
    async fn incidents_reject_unparseable_params() {
        let (base, _store) = spawn().await;
        for (param, value) in [
            ("since", "garbage"),
            ("until", "garbage"),
            ("monitor_id", "seven"),
            ("limit", "abc"),
        ] {
            assert_rejects(format!("{base}/api/v1/incidents?{param}={value}"), param).await;
        }
    }

    /// The regression that motivated this: `Date.now()` milliseconds parse as a
    /// valid integer, so the endpoint used to answer 200 with `[]` — a wrong
    /// request masquerading as a quiet week.
    #[tokio::test]
    async fn incidents_reject_millisecond_timestamps() {
        let (base, store) = spawn().await;
        let m = monitor_named(&store, "a").await;
        seed_incident(&store, m.id, 3600).await;
        assert_rejects(
            format!("{base}/api/v1/incidents?since=99999999999"),
            "since",
        )
        .await;
    }

    #[tokio::test]
    async fn incidents_reject_since_after_until() {
        let (base, _store) = spawn().await;
        let now = now_epoch();
        let resp = reqwest::get(format!(
            "{base}/api/v1/incidents?since={}&until={}",
            now,
            now - 3600
        ))
        .await
        .unwrap();
        assert_eq!(resp.status(), 400);
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("\"since\"") && body.contains("\"until\""),
            "{body}"
        );
    }

    #[tokio::test]
    async fn incidents_absent_params_use_defaults() {
        let (base, store) = spawn().await;
        let m = monitor_named(&store, "a").await;
        seed_incident(&store, m.id, 3600).await;
        // Outside the default 7d window.
        seed_incident(&store, m.id, 10 * 86_400).await;

        let body: Value = reqwest::get(format!("{base}/api/v1/incidents"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body.as_array().unwrap().len(), 1);
    }

    /// Unknown parameters stay ignored on purpose — do not "fix" this into a
    /// rejection; forward compatibility depends on it.
    #[tokio::test]
    async fn incidents_ignore_unknown_params() {
        let (base, store) = spawn().await;
        let m = monitor_named(&store, "a").await;
        seed_incident(&store, m.id, 3600).await;
        seed_incident(&store, m.id, 10 * 86_400).await;

        let resp = reqwest::get(format!("{base}/api/v1/incidents?days=7"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.unwrap();
        // Still the default 7d window, not something derived from `days`.
        assert_eq!(body.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn prometheus_inspect_returns_metric_map() {
        let metrics_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("# TYPE up gauge\nup{job=\"a\"} 1\n"),
            )
            .mount(&metrics_server)
            .await;
        let (base, _store) = spawn().await;
        let body: Value = reqwest::Client::new()
            .post(format!("{base}/api/v1/checks/prometheus/inspect"))
            .json(&json!({ "url": metrics_server.uri() }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            body["metrics"]["up"]["labels"]["job"]
                .as_array()
                .unwrap()
                .contains(&json!("a"))
        );
    }

    #[tokio::test]
    async fn prometheus_inspect_unreachable_is_502() {
        let (base, _store) = spawn().await;
        let resp = reqwest::Client::new()
            .post(format!("{base}/api/v1/checks/prometheus/inspect"))
            .json(&json!({ "url": "http://127.0.0.1:1/metrics", "timeout_secs": 1 }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 502);
    }
}
