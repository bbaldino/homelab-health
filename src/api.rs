use crate::check::{ConfigSchema, Registry};
use crate::store::{Monitor, MonitorStatus, NewMonitor, Store};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
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
}
