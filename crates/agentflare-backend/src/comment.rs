use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemComment {
    pub id: String,
    pub item_id: String,
    pub author_agent: String,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
}

fn row_to_comment(row: &rusqlite::Row) -> rusqlite::Result<ItemComment> {
    Ok(ItemComment {
        id: row.get(0)?,
        item_id: row.get(1)?,
        author_agent: row.get(2)?,
        body: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Create a comment on an item. `author_agent` is the identity of the caller.
pub fn create(
    conn: &Connection,
    item_id: &str,
    author_agent: &str,
    body: &str,
) -> Result<ItemComment> {
    let id = db_kit::ids::new_id();
    let ts = now();
    conn.execute(
        "INSERT INTO item_comments (id, item_id, author_agent, body, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![id, item_id, author_agent, body, ts, ts],
    )?;
    get(conn, &id)
}

/// Get a single comment by id.
pub fn get(conn: &Connection, id: &str) -> Result<ItemComment> {
    conn.query_row(
        "SELECT id, item_id, author_agent, body, created_at, updated_at
         FROM item_comments WHERE id = ?1",
        rusqlite::params![id],
        row_to_comment,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => crate::error::Error::NotFound(id.to_string()),
        other => other.into(),
    })
}

/// Update the body of a comment. Returns the updated comment.
pub fn update(conn: &Connection, id: &str, body: &str) -> Result<ItemComment> {
    let ts = now();
    let changed = conn.execute(
        "UPDATE item_comments SET body = ?2, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![id, body, ts],
    )?;
    if changed == 0 {
        return Err(crate::error::Error::NotFound(id.to_string()));
    }
    get(conn, id)
}

/// Delete a comment by id.
pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let changed = conn.execute(
        "DELETE FROM item_comments WHERE id = ?1",
        rusqlite::params![id],
    )?;
    if changed == 0 {
        return Err(crate::error::Error::NotFound(id.to_string()));
    }
    Ok(())
}

/// List all comments for an item, oldest first.
pub fn list_by_item(conn: &Connection, item_id: &str) -> Result<Vec<ItemComment>> {
    let mut stmt = conn.prepare(
        "SELECT id, item_id, author_agent, body, created_at, updated_at
         FROM item_comments WHERE item_id = ?1 ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![item_id], row_to_comment)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Check if this comment is the latest (most recent) on its item.
pub fn is_latest(conn: &Connection, comment: &ItemComment) -> Result<bool> {
    // `created_at` is second-resolution, so two comments posted in the same
    // second tie on MAX(created_at) — comparing timestamps alone would treat
    // both as "latest". Break ties with `id` (UUIDv7, time-ordered), which
    // reflects true insertion order even within one second.
    let latest_id: Option<String> = conn
        .query_row(
            "SELECT id FROM item_comments WHERE item_id = ?1
             ORDER BY created_at DESC, id DESC LIMIT 1",
            rusqlite::params![comment.item_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(crate::error::Error::Database)?;
    Ok(latest_id.is_none_or(|id| id == comment.id))
}
