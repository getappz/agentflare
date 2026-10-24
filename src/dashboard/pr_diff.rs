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
pub async fn diff_context_handler(Query(q): Query<PrDiffContextQuery>) -> Response {
    let repo = crate::github::RepoId {
        owner: q.owner,
        repo: q.repo,
    };
    let text = match crate::github::Client::new() {
        Ok(client) => match crate::github::pulls::list_files(&client, &repo, q.number) {
            Ok(files) => {
                let side = match q.side.as_deref() {
                    Some("left") | Some("LEFT") => crate::review::diff_display::AnchorSide::Left,
                    _ => crate::review::diff_display::AnchorSide::Right,
                };
                let anchor = crate::review::diff_display::Anchor {
                    line_start: q.line,
                    line_end: q.line,
                    side,
                };
                crate::dashboard::data::pr_diff_context_plain(&files, &q.path, &anchor)
            }
            Err(e) => format!("no context for {} (fetch failed: {e})", q.path),
        },
        Err(_) => format!("no context for {}", q.path),
    };
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
}
