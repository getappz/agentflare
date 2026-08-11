use rusqlite::Connection;

use crate::error::Result;

use super::{Item, row_to_item};

/// FTS5 search across items (name, description, metadata) within a project.
/// Returns BM25-ranked results, most relevant first. Query is sanitised
/// via `flare-search-kit` into safe FTS5 tokens (quoted, operators
/// neutralised) so user input like `PR-123` isn't misinterpreted as
/// column:value syntax.
///
/// Falls back to a `LIKE` substring scan when FTS5 finds nothing. FTS5's
/// default tokenizer splits on `-`/`_`, so a compound identifier like
/// `agentflare-store` indexes as separate `agentflare`/`store` tokens —
/// a query for `flare-store` (or bare `flare`) would otherwise miss it,
/// since `flare` is a suffix, not a prefix, of `agentflare`.
pub fn search(
    conn: &Connection,
    project_id: &str,
    query: &str,
    limit: Option<usize>,
) -> Result<Vec<Item>> {
    let limit = limit.unwrap_or(20);
    let safe =
        flare_search_kit::fts_query(query, flare_search_kit::MatchMode::All).unwrap_or_default();
    if safe.is_empty() {
        return Ok(vec![]);
    }
    let mut stmt = conn.prepare(
        "SELECT items.id, items.project_id, items.state_id, items.name, items.description,
                items.priority, items.parent_id, items.assignee_agent, items.sequence_id,
                items.sort_order, items.started_at, items.completed_at, items.archived_at,
                items.external_source, items.external_id, items.metadata,
                items.created_at, items.updated_at, items.deleted_at
         FROM items_fts
         JOIN items ON items.rowid = items_fts.rowid
         WHERE items.project_id = ?1
           AND items_fts MATCH ?2
           AND items.deleted_at IS NULL
         ORDER BY bm25(items_fts, 3.0, 1.0, 1.0)
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![project_id, safe, flare_search_kit::clamped_limit(limit)],
        row_to_item,
    )?;
    let results: Vec<Item> = rows.collect::<std::result::Result<_, _>>()?;
    if !results.is_empty() {
        return Ok(results);
    }

    let like_pat = format!(
        "%{}%",
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut like_stmt = conn.prepare(
        "SELECT items.id, items.project_id, items.state_id, items.name, items.description,
                items.priority, items.parent_id, items.assignee_agent, items.sequence_id,
                items.sort_order, items.started_at, items.completed_at, items.archived_at,
                items.external_source, items.external_id, items.metadata,
                items.created_at, items.updated_at, items.deleted_at
         FROM items
         WHERE items.project_id = ?1
           AND items.deleted_at IS NULL
           AND (items.name LIKE ?2 ESCAPE '\\' OR items.description LIKE ?2 ESCAPE '\\')
         ORDER BY items.updated_at DESC
         LIMIT ?3",
    )?;
    let like_rows = like_stmt.query_map(
        rusqlite::params![project_id, like_pat, flare_search_kit::clamped_limit(limit)],
        row_to_item,
    )?;
    Ok(like_rows.collect::<std::result::Result<_, _>>()?)
}
