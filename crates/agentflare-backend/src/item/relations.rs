use rusqlite::Connection;

use crate::error::Result;

use super::{get, workspace_id_for_project};

pub fn add_label(conn: &Connection, item_id: &str, label_id: &str) -> Result<()> {
    // A label may only be attached to an item in the same scope: a project-scoped
    // label must share the item's project; a workspace-level label (project_id NULL)
    // must share the item's workspace. This mirrors Plane's project-membership check
    // and, because item::create routes through here, guards that path too.
    let item = get(conn, item_id)?;
    let label = crate::label::get(conn, label_id)?;
    let in_scope = match &label.project_id {
        Some(project_id) => project_id == &item.project_id,
        None => label.workspace_id == workspace_id_for_project(conn, &item.project_id)?,
    };
    if !in_scope {
        return Err(crate::error::Error::Validation(format!(
            "label {label_id} is not in item {item_id}'s scope (project or workspace)"
        )));
    }
    conn.execute(
        "INSERT OR IGNORE INTO item_labels (item_id, label_id) VALUES (?1, ?2)",
        rusqlite::params![item_id, label_id],
    )?;
    Ok(())
}

pub fn remove_label(conn: &Connection, item_id: &str, label_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM item_labels WHERE item_id = ?1 AND label_id = ?2",
        rusqlite::params![item_id, label_id],
    )?;
    Ok(())
}

pub fn list_labels(conn: &Connection, item_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT label_id FROM item_labels WHERE item_id = ?1")?;
    let rows = stmt.query_map(rusqlite::params![item_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn add_assignee(conn: &Connection, item_id: &str, agent_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO item_assignees (item_id, agent_id) VALUES (?1, ?2)",
        rusqlite::params![item_id, agent_id],
    )?;
    Ok(())
}

pub fn remove_assignee(conn: &Connection, item_id: &str, agent_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM item_assignees WHERE item_id = ?1 AND agent_id = ?2",
        rusqlite::params![item_id, agent_id],
    )?;
    Ok(())
}

pub fn list_assignees(conn: &Connection, item_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT agent_id FROM item_assignees WHERE item_id = ?1")?;
    let rows = stmt.query_map(rusqlite::params![item_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn add_dependency(conn: &Connection, item_id: &str, depends_on: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO item_dependencies (item_id, depends_on_item_id) VALUES (?1, ?2)",
        rusqlite::params![item_id, depends_on],
    )?;
    Ok(())
}

pub fn remove_dependency(conn: &Connection, item_id: &str, depends_on: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM item_dependencies WHERE item_id = ?1 AND depends_on_item_id = ?2",
        rusqlite::params![item_id, depends_on],
    )?;
    Ok(())
}

pub fn list_dependencies(conn: &Connection, item_id: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT depends_on_item_id FROM item_dependencies WHERE item_id = ?1")?;
    let rows = stmt.query_map(rusqlite::params![item_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Dependency edges for a set of items, with each edge's target state_group
/// already joined in — so a caller's blocking status is correct even when
/// the dependency target isn't itself in the same shortlist/limit window
/// (e.g. a completed dependency that fell outside `groom`'s cap must not
/// read back as an open blocker just because its state wasn't looked up).
/// `(item_id, depends_on_item_id, depends_on_state_group)`.
pub fn dependency_edges_for_items(
    conn: &Connection,
    item_ids: &[String],
) -> Result<Vec<(String, String, String)>> {
    if item_ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = item_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT d.item_id, d.depends_on_item_id, s.group_name
         FROM item_dependencies d
         JOIN items i ON i.id = d.depends_on_item_id AND i.deleted_at IS NULL
         JOIN states s ON s.id = i.state_id
         WHERE d.item_id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(item_ids.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Fan-in counts: for each of `item_ids`, how many other (non-deleted) items
/// declare a dependency on it — project-wide, not limited to the same
/// shortlist/limit window a caller happens to be looking at.
pub fn dependency_fanin_for_items(
    conn: &Connection,
    item_ids: &[String],
) -> Result<std::collections::HashMap<String, i64>> {
    if item_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = item_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT d.depends_on_item_id, COUNT(*)
         FROM item_dependencies d
         JOIN items i ON i.id = d.item_id AND i.deleted_at IS NULL
         WHERE d.depends_on_item_id IN ({placeholders})
         GROUP BY d.depends_on_item_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(item_ids.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
