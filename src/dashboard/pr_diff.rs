use axum::{
    Router,
    extract::Query,
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;

pub fn router() -> Router {
    Router::new().route("/api/pr/diff-context", get(diff_context_handler))
}

#[derive(Deserialize)]
pub struct PrDiffContextQuery {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub path: String,
    pub line: u32,
    /// `left` or `right` (GitHub `diffSide`); defaults to right.
    pub side: Option<String>,
}

/// Anchored side-by-side diff excerpt for the read-only dashboard PR view.
///
/// The GitHub client and the paginated file fetch are blocking, so they run on
/// `spawn_blocking` rather than on the async dashboard worker (same shape as
/// `dashboard::artifacts::index`).
pub async fn diff_context_handler(Query(q): Query<PrDiffContextQuery>) -> Response {
    let repo = crate::github::RepoId {
        owner: q.owner,
        repo: q.repo,
    };
    let number = q.number;
    let path = q.path;
    let line = q.line;
    let side = match q.side.as_deref() {
        Some("left") | Some("LEFT") => crate::review::diff_display::AnchorSide::Left,
        _ => crate::review::diff_display::AnchorSide::Right,
    };
    let fallback_path = path.clone();
    let text = tokio::task::spawn_blocking(move || {
        let client = match crate::github::Client::new() {
            Ok(client) => client,
            Err(_) => return format!("no context for {path}"),
        };
        match crate::github::pulls::list_files(&client, &repo, number) {
            Ok(files) => {
                let anchor = crate::review::diff_display::Anchor {
                    line_start: line,
                    line_end: line,
                    side,
                };
                crate::dashboard::data::pr_diff_context_plain(&files, &path, &anchor)
            }
            Err(e) => format!("no context for {path} (fetch failed: {e})"),
        }
    })
    .await
    .unwrap_or_else(|_| format!("no context for {fallback_path}"));
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
}
