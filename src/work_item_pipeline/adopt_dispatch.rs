// Adopt-path helpers: repair-dispatch comment detection and task overlay when
// a new job adopts a non-terminal run (`include!`d into work_item_pipeline.rs).

/// Supervisor repair-dispatch comment prefixes (must stay in sync with
/// `src/supervisor.rs` / `src/supervisor/review_bots.rs`).
const REPAIR_DISPATCH_MARKERS: &[&str] = &[
    "## supervisor — CodeRabbit review repair dispatched",
    "## supervisor — CI self-repair dispatched",
    "## supervisor — merge-conflict repair dispatched",
];

/// Item-metadata key: `created_at` of the newest repair-dispatch comment this
/// item's pipeline already adopted onto its workflow run (item #687).
pub(crate) const REPAIR_DISPATCH_ADOPTED_AT_KEY: &str = "repair_dispatch_adopted_at";

fn repair_dispatch_adopted_cursor(mcp: &AgentflareMcp, item_id: &str) -> i64 {
    let Ok(raw) = mcp.item_get(crate::mcp_server::types::ItemRequest {
        action: "get".into(),
        id: Some(item_id.to_string()),
        ..Default::default()
    }) else {
        return 0;
    };
    let Ok(item) = serde_json::from_str::<agentflare_backend::item::Item>(&raw) else {
        return 0;
    };
    crate::mcp_server::metadata_object(&item.metadata)
        .get(REPAIR_DISPATCH_ADOPTED_AT_KEY)
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// Newest unadopted item comment that announces a supervisor repair dispatch.
pub(crate) fn latest_repair_dispatch_body(
    mcp: &AgentflareMcp,
    item_id: &str,
) -> Option<(String, i64)> {
    let after = repair_dispatch_adopted_cursor(mcp, item_id);
    let comments = match mcp.with_backend_db(|conn| {
        agentflare_backend::comment::list_by_item(conn, item_id)
    }) {
        Ok(Ok(comments)) => comments,
        _ => return None,
    };
    comments
        .iter()
        .filter(|c| {
            c.created_at > after
                && REPAIR_DISPATCH_MARKERS
                    .iter()
                    .any(|m| c.body.starts_with(m))
        })
        .max_by_key(|c| c.created_at)
        .map(|c| (c.body.clone(), c.created_at))
}

pub(crate) fn persist_repair_dispatch_adopted_at(
    mcp: &AgentflareMcp,
    item_id: &str,
    created_at: i64,
) {
    let _ = mcp.with_backend_db(|conn| {
        crate::mcp_server::merge_item_metadata(conn, item_id, |meta| {
            meta.insert(
                REPAIR_DISPATCH_ADOPTED_AT_KEY.into(),
                serde_json::Value::from(created_at),
            );
        })
    });
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
