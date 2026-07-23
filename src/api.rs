use crate::check::{ConfigSchema, Registry};
use crate::status::Status;
use crate::store::{Monitor, MonitorStatus, NewMonitor, Sample, Store};
use crate::uptime::{Uptime, compute_uptime};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct ApiState {
    pub store: Store,
    pub registry: Arc<Registry>,
}

fn internal(e: sqlx::Error) -> StatusCode {
    tracing::error!("db error: {e}");
    StatusCode::INTERNAL_SERVER_ERROR
}

pub fn build_app(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/check-types", get(check_types))
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
        .fallback(crate::ui::serve_asset)
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

async fn list_monitors(State(state): State<ApiState>) -> Result<Json<Vec<Monitor>>, StatusCode> {
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
    match state
        .store
        .update_monitor(id, body)
        .await
        .map_err(internal)?
    {
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
    if state
        .store
        .get_monitor(id)
        .await
        .map_err(internal)?
        .is_none()
    {
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
    if state
        .store
        .get_monitor(id)
        .await
        .map_err(internal)?
        .is_none()
    {
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
        assert_eq!(arr.len(), 6);
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
}
