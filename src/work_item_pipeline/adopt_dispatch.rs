// Adopt-path helpers: repair-dispatch comment detection and task overlay when
// a new job adopts a non-terminal run (`include!`d into work_item_pipeline.rs).

/// Supervisor repair-dispatch comment prefixes (must stay in sync with
/// `src/supervisor.rs` / `src/supervisor/review_bots.rs`).
const REPAIR_DISPATCH_MARKERS: &[&str] = &[
    "## supervisor — CodeRabbit review repair dispatched",
    "## supervisor — CI self-repair dispatched",
    "## supervisor — merge-conflict repair dispatched",
];

/// Newest item comment that announces a supervisor repair dispatch, if any.
pub(crate) fn latest_repair_dispatch_body(
    mcp: &AgentflareMcp,
    item_id: &str,
) -> Option<String> {
    let comments = match mcp.with_backend_db(|conn| {
        agentflare_backend::comment::list_by_item(conn, item_id)
    }) {
        Ok(Ok(comments)) => comments,
        _ => return None,
    };
    comments
        .iter()
        .filter(|c| {
            REPAIR_DISPATCH_MARKERS
                .iter()
                .any(|m| c.body.starts_with(m))
        })
        .max_by_key(|c| c.created_at)
        .map(|c| c.body.clone())
}

/// Single SDD task carrying a repair dispatch comment body.
pub(crate) fn repair_dispatch_task(body: String) -> SddTask {
    SddTask {
        id: 0,
        title: "Repair review findings".into(),
        body,
        model_tier: None,
    }
}
