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
        "INSERT OR IGNORE INTO item_dependencies (item_id, depends_on_item_id, relation_type) VALUES (?1, ?2, 'blocks')",
        rusqlite::params![item_id, depends_on],
    )?;
    Ok(())
}

pub fn remove_dependency(conn: &Connection, item_id: &str, depends_on: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM item_dependencies WHERE item_id = ?1 AND depends_on_item_id = ?2 AND relation_type = 'blocks'",
        rusqlite::params![item_id, depends_on],
    )?;
    Ok(())
}

pub fn list_dependencies(conn: &Connection, item_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT depends_on_item_id FROM item_dependencies WHERE item_id = ?1 AND relation_type = 'blocks'",
    )?;
    let rows = stmt.query_map(rusqlite::params![item_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Items (non-deleted) with a live dependency on `item_id` — the reverse
/// direction of `list_dependencies`. Used to find who might unblock once
/// `item_id` completes (item #195's auto-dispatch-dependents cascade).
pub fn dependents_of(conn: &Connection, item_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT d.item_id FROM item_dependencies d
         JOIN items i ON i.id = d.item_id AND i.deleted_at IS NULL
         WHERE d.depends_on_item_id = ?1 AND d.relation_type = 'blocks'",
    )?;
    let rows = stmt.query_map(rusqlite::params![item_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// True when `item_id` has at least one dependency and every one of them
/// (via `dependency_edges_for_items`, so a deleted dependency target counts
/// as unsatisfied rather than silently dropping out of the count) is in the
/// `completed` state group. An item with no dependencies at all is not a
/// dependent in the first place, so this returns `false` for it rather than
/// vacuously true -- callers only reach this after `dependents_of` already
/// confirmed at least one dependency edge exists.
pub fn all_dependencies_completed(conn: &Connection, item_id: &str) -> Result<bool> {
    let deps = list_dependencies(conn, item_id)?;
    if deps.is_empty() {
        return Ok(false);
    }
    let edges = dependency_edges_for_items(conn, &[item_id.to_string()])?;
    Ok(edges.len() == deps.len() && edges.iter().all(|(_, _, group)| group == "completed"))
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
         WHERE d.item_id IN ({placeholders}) AND d.relation_type = 'blocks'"
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
         WHERE d.depends_on_item_id IN ({placeholders}) AND d.relation_type = 'blocks'
         GROUP BY d.depends_on_item_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(item_ids.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The three stored relation types (see `0012_item_relation_types.sql`).
/// `blocks` is directional (matches `add_dependency`'s existing semantics);
/// `duplicate` and `relates_to` are symmetric.
pub const RELATION_TYPES: [&str; 3] = ["blocks", "duplicate", "relates_to"];

/// Canonicalize a symmetric-relation pair's storage order so `(A, B)` and
/// `(B, A)` always land as the same row instead of two independent ones.
/// Only meaningful for symmetric types -- callers handle `blocks`
/// separately, since it must stay directional.
fn canonicalize_pair<'a>(item_id: &'a str, other_id: &'a str) -> (&'a str, &'a str) {
    if item_id <= other_id {
        (item_id, other_id)
    } else {
        (other_id, item_id)
    }
}

/// Add a relation of any type. `blocks` delegates to `add_dependency`
/// unchanged (directional, zero behavior change). Symmetric types
/// (`duplicate`, `relates_to`) canonicalize storage order first so either
/// insertion order produces the same row.
pub fn add_relation(
    conn: &Connection,
    item_id: &str,
    other_id: &str,
    relation_type: &str,
) -> Result<()> {
    if relation_type == "blocks" {
        return add_dependency(conn, item_id, other_id);
    }
    let (a, b) = canonicalize_pair(item_id, other_id);
    conn.execute(
        "INSERT OR IGNORE INTO item_dependencies (item_id, depends_on_item_id, relation_type) VALUES (?1, ?2, ?3)",
        rusqlite::params![a, b, relation_type],
    )?;
    Ok(())
}

/// Remove a relation of any type -- the inverse of `add_relation`, with the
/// same `blocks`-delegates / symmetric-canonicalize split.
pub fn remove_relation(
    conn: &Connection,
    item_id: &str,
    other_id: &str,
    relation_type: &str,
) -> Result<()> {
    if relation_type == "blocks" {
        return remove_dependency(conn, item_id, other_id);
    }
    let (a, b) = canonicalize_pair(item_id, other_id);
    conn.execute(
        "DELETE FROM item_dependencies WHERE item_id = ?1 AND depends_on_item_id = ?2 AND relation_type = ?3",
        rusqlite::params![a, b, relation_type],
    )?;
    Ok(())
}

/// Other item IDs related to `item_id` by `relation_type`. `blocks` matches
/// `list_dependencies`'s directional semantics (what `item_id` depends on).
/// Symmetric types read from either column, so `(A, B)` and `(B, A)`
/// produce the identical result from either item's perspective.
pub fn list_relations_by_type(
    conn: &Connection,
    item_id: &str,
    relation_type: &str,
) -> Result<Vec<String>> {
    if relation_type == "blocks" {
        return list_dependencies(conn, item_id);
    }
    let mut stmt = conn.prepare(
        "SELECT CASE WHEN item_id = ?1 THEN depends_on_item_id ELSE item_id END
         FROM item_dependencies
         WHERE (item_id = ?1 OR depends_on_item_id = ?1) AND relation_type = ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![item_id, relation_type], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// `(relation_type, other_item_id)` pairs across all three types, for a
/// detail panel to render everything about `item_id` in one call.
pub fn list_all_relations(conn: &Connection, item_id: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for relation_type in RELATION_TYPES {
        for other_id in list_relations_by_type(conn, item_id, relation_type)? {
            out.push((relation_type.to_string(), other_id));
        }
    }
    Ok(out)
}
