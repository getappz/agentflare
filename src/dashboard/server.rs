use axum::{
    Router,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "dashboard/web/"]
struct WebAssets;

async fn claims_handler() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], crate::dashboard::data::claims_json())
        .into_response()
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match WebAssets::get(path) {
        Some(c) => ([(header::CONTENT_TYPE, mime_for(path))], c.data.into_owned()).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn mime_for(p: &str) -> &'static str {
    if p.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if p.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if p.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

pub fn router() -> Router {
    Router::new().route("/api/claims", get(claims_handler)).fallback(static_handler)
}

pub async fn run(host: &str, port: u16, open: bool) {
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .expect("failed to bind dashboard server");
    let addr = listener.local_addr().expect("no local addr");
    let url = format!("http://{addr}");
    eprintln!("agentflare dashboard listening on {url}");
    if host != "127.0.0.1" && host != "localhost" {
        eprintln!("  warning: bound to {host} — anyone on your network can view this");
    }
    if open {
        crate::dashboard::open_browser(&url);
    }
    axum::serve(listener, router()).await.expect("dashboard server error");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn claims_endpoint_returns_json_array() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router()).await.unwrap(); });
        let body = reqwest::get(format!("http://{addr}/api/claims"))
            .await.unwrap().text().await.unwrap();
        assert!(body.starts_with('['), "expected JSON array, got: {body}");
    }
}
