use homelab_health::api::{ApiState, build_app};
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
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env();
    tracing::info!("connecting to {}", config.db_url);
    let store = Store::connect(&config.db_url).await?;
    let registry = Arc::new(Registry::with_builtins());

    // Background scheduler.
    let scheduler = Scheduler::new(store.clone(), registry.clone(), DEBOUNCE_THRESHOLD)
        .retention_days(config.retention_days);
    tokio::spawn(scheduler.run());

    // HTTP API.
    let state = ApiState { store, registry };
    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("listening on {}", config.bind);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => tracing::error!("failed to install SIGTERM handler: {e}"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received, stopping");
}
