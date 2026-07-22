use crate::check::{ConfigSchema, Registry};
use crate::store::{Monitor, NewMonitor, Store};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
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
