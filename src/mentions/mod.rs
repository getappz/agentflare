//! `@I:uuid` / `@A:uuid` / `@search:"query"` inline references. Parsed out of
//! the user's prompt in `hook.rs::prompt_submit`, resolved against the
//! backend DB, and injected into `additionalContext` so the model doesn't
//! have to make a separate item/asset/search tool call for content the user
//! already pointed at directly.

pub mod format;
pub mod parse;
pub mod resolve;

pub use format::format_context;
pub use parse::parse_mentions;
pub use resolve::resolve_mentions;

/// Entry point for `hook.rs::prompt_submit`. Opens its own backend-db
/// connection (the hook runs standalone, with no live `AgentflareMcp`
/// instance to borrow one from) and resolves mentions scoped to this repo's
/// linked project. Returns `None` when there's nothing to expand.
pub fn expand(prompt: &str) -> Option<String> {
    let mentions = parse_mentions(prompt);
    if mentions.is_empty() {
        return None;
    }
    let db_path = crate::paths::home().join(".agentflare").join("backend.db");
    if !db_path.exists() {
        return None;
    }
    let conn = agentflare_backend::db::open_db(&db_path).ok()?;
    let project_id = crate::mcp_server::AgentflareMcp::default()
        .resolve_project(&conn)
        .ok()?
        .id;
    let resolved = resolve_mentions(&conn, &project_id, &mentions);
    if resolved.is_empty() {
        return None;
    }
    Some(format_context(&resolved))
}
