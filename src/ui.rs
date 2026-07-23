use axum::http::{StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};

#[derive(rust_embed::RustEmbed)]
#[folder = "ui/dist/"]
struct Assets;

const FALLBACK_HTML: &str = "<!doctype html><title>homelab-health</title><h1>homelab-health</h1>\
     <p>UI not built. Run <code>npm --prefix ui run build</code>, or use the JSON API at <code>/api/v1/status</code>.</p>";

/// Serve an embedded static asset; fall back to index.html for SPA routes,
/// and to a minimal inline page when the UI has not been built.
pub async fn serve_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(content) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return ([(header::CONTENT_TYPE, mime.as_ref())], content.data).into_response();
    }
    // SPA fallback: serve index.html for client-side routes.
    match Assets::get("index.html") {
        Some(index) => Html(index.data.to_vec()).into_response(),
        None => (StatusCode::OK, Html(FALLBACK_HTML)).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serves_fallback_when_ui_absent_returns_html() {
        // In debug/test builds rust-embed reads ui/dist from disk; if it's not
        // built, we still return a 200 HTML page (never a 500/panic).
        let resp = serve_asset("/".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unmatched_api_path_returns_404() {
        let resp = serve_asset("/api/v1/nope".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
