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
    // `created_at` is second-resolution, so two comments posted in the same
    // second tie on it — broken by SQLite's implicit `rowid`, not `id`: `id`
    // is a random nanoid (`db_kit::ids::new_id`), not time-ordered, so tying
    // on it scrambled same-second comments into an arbitrary order. `rowid`
    // is monotonically assigned on insert (this table has no explicit
    // `INTEGER PRIMARY KEY`/`WITHOUT ROWID`, so it's SQLite's own implicit
    // one) and so reflects true insertion order even within one second.
    let mut stmt = conn.prepare(
        "SELECT id, item_id, author_agent, body, created_at, updated_at
         FROM item_comments WHERE item_id = ?1 ORDER BY created_at ASC, rowid ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![item_id], row_to_comment)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Check if this comment is the latest (most recent) on its item.
pub fn is_latest(conn: &Connection, comment: &ItemComment) -> Result<bool> {
    // Same same-second tie-breaking rationale as `list_by_item` above.
    let latest_id: Option<String> = conn
        .query_row(
            "SELECT id FROM item_comments WHERE item_id = ?1
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            rusqlite::params![comment.item_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(crate::error::Error::Database)?;
    Ok(latest_id.is_none_or(|id| id == comment.id))
}

/// Comment counts per item id, batched into one query — powers
/// `item(list, has_comments=true)` without an N+1 per-item lookup. Items
/// with zero comments are simply absent from the returned map.
pub fn count_by_items(
    conn: &Connection,
    item_ids: &[String],
) -> Result<std::collections::HashMap<String, i64>> {
    if item_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = item_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT item_id, COUNT(*) FROM item_comments WHERE item_id IN ({placeholders}) GROUP BY item_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(item_ids.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::item::{self, CreateItem};
    use crate::project::{self, CreateProject};
    use crate::workspace::{self, CreateWorkspace};

    fn seed_item(conn: &Connection, suffix: &str) -> String {
        let ws = workspace::create(
            conn,
            CreateWorkspace {
                name: format!("Test{suffix}"),
                slug: format!("test{suffix}"),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = project::create(
            conn,
            CreateProject {
                workspace_id: ws.id,
                name: format!("Test{suffix}"),
                identifier: format!("T{suffix}"),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let state_id = crate::state::list_by_project(conn, &proj.id)
            .unwrap()
            .into_iter()
            .find(|s| s.is_default)
            .unwrap()
            .id;
        item::create(
            conn,
            CreateItem {
                project_id: proj.id,
                state_id,
                name: format!("Item{suffix}"),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
                start_date: None,
                due_date: None,
            },
        )
        .unwrap()
        .id
    }

    #[test]
    fn count_by_items_batches_counts_and_omits_zero_comment_items() {
        let conn = db::open_in_memory().unwrap();
        let commented = seed_item(&conn, "A");
        let uncommented = seed_item(&conn, "B");
        create(&conn, &commented, "agent-1", "first").unwrap();
        create(&conn, &commented, "agent-1", "second").unwrap();

        let counts = count_by_items(&conn, &[commented.clone(), uncommented.clone()]).unwrap();

        assert_eq!(counts.get(&commented), Some(&2));
        assert_eq!(
            counts.get(&uncommented),
            None,
            "items with zero comments are absent from the map, not zero-valued"
        );
    }

    #[test]
    fn count_by_items_empty_input_returns_empty_map_without_querying() {
        let conn = db::open_in_memory().unwrap();
        let counts = count_by_items(&conn, &[]).unwrap();
        assert!(counts.is_empty());
    }
}
