use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::events;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub project_id: String,
    pub state_id: String,
    pub name: String,
    pub description: String,
    pub priority: String,
    pub parent_id: Option<String>,
    pub assignee_agent: Option<String>,
    pub sequence_id: i64,
    pub sort_order: f64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub archived_at: Option<i64>,
    pub external_source: Option<String>,
    pub external_id: Option<String>,
    pub metadata: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateItem {
    pub project_id: String,
    pub state_id: String,
    pub name: String,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub parent_id: Option<String>,
    pub assignee_agent: Option<String>,
    pub sort_order: Option<f64>,
    pub external_source: Option<String>,
    pub external_id: Option<String>,
    pub metadata: Option<String>,
    pub label_ids: Vec<String>,
    pub assignee_ids: Vec<String>,
    pub dependency_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateItem {
    pub name: Option<String>,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub state_id: Option<String>,
    pub assignee_agent: Option<String>,
    pub sort_order: Option<f64>,
    pub metadata: Option<String>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        project_id: row.get(1)?,
        state_id: row.get(2)?,
        name: row.get(3)?,
        description: row.get(4)?,
        priority: row.get(5)?,
        parent_id: row.get(6)?,
        assignee_agent: row.get(7)?,
        sequence_id: row.get(8)?,
        sort_order: row.get(9)?,
        started_at: row.get(10)?,
        completed_at: row.get(11)?,
        archived_at: row.get(12)?,
        external_source: row.get(13)?,
        external_id: row.get(14)?,
        metadata: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
        deleted_at: row.get(18)?,
    })
}

fn next_sequence_id(conn: &Connection, project_id: &str) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO project_sequences (project_id, next_seq) VALUES (?1, 1)
         ON CONFLICT(project_id) DO UPDATE SET next_seq = next_seq + 1",
        rusqlite::params![project_id],
    )?;
    conn.query_row(
        "SELECT next_seq FROM project_sequences WHERE project_id = ?1",
        rusqlite::params![project_id],
        |row| row.get(0),
    )
}

fn workspace_id_for_project(conn: &Connection, project_id: &str) -> Result<String> {
    conn.query_row(
        "SELECT workspace_id FROM projects WHERE id = ?1 AND deleted_at IS NULL",
        rusqlite::params![project_id],
        |row| row.get(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
            crate::error::Error::NotFound(project_id.to_string())
        }
        other => other.into(),
    })
}

pub fn create(conn: &Connection, input: CreateItem) -> Result<Item> {
    let id = db_kit::ids::new_id();
    let ts = now();
    let sort_order = input.sort_order.unwrap_or(65535.0);
    let description = input.description.unwrap_or_default();
    let priority = input.priority.unwrap_or_else(|| "none".to_string());
    let metadata = input.metadata.unwrap_or_else(|| "{}".to_string());
    let assignee_agent = input
        .assignee_agent
        .as_deref()
        .map(agent_registry::canonicalize);

    let state = crate::state::get(conn, &input.state_id)?;
    if state.project_id != input.project_id {
        return Err(crate::error::Error::InvalidTransition(format!(
            "state {} belongs to a different project than project {}",
            input.state_id, input.project_id
        )));
    }

    let tx = conn.unchecked_transaction()?;
    let seq = next_sequence_id(&tx, &input.project_id)?;
    tx.execute(
        "INSERT INTO items (id, project_id, state_id, name, description, priority, parent_id, assignee_agent, sequence_id, sort_order, external_source, external_id, metadata, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        rusqlite::params![
            id,
            input.project_id,
            input.state_id,
            input.name,
            description,
            priority,
            input.parent_id,
            assignee_agent,
            seq,
            sort_order,
            input.external_source,
            input.external_id,
            metadata,
            ts,
            ts,
        ],
    )?;
    for label_id in &input.label_ids {
        add_label(&tx, &id, label_id)?;
    }
    for agent_id in &input.assignee_ids {
        add_assignee(&tx, &id, agent_id)?;
    }
    for dep_id in &input.dependency_ids {
        add_dependency(&tx, &id, dep_id)?;
    }
    tx.commit()?;
    let item = get(conn, &id)?;
    if let Ok(wid) = workspace_id_for_project(conn, &item.project_id) {
        events::emit(
            conn,
            &wid,
            "item",
            "create",
            serde_json::to_value(&item).unwrap_or_default(),
        );
    }
    Ok(item)
}

pub fn get(conn: &Connection, id: &str) -> Result<Item> {
    conn.query_row(
        "SELECT id, project_id, state_id, name, description, priority, parent_id, assignee_agent, sequence_id, sort_order, started_at, completed_at, archived_at, external_source, external_id, metadata, created_at, updated_at, deleted_at
         FROM items WHERE id = ?1 AND deleted_at IS NULL",
        rusqlite::params![id],
        row_to_item,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => crate::error::Error::NotFound(id.to_string()),
        other => other.into(),
    })
}

/// Resolve a user-supplied identifier to an item UUID.
/// Accepts a UUID (pass-through) or a numeric `sequence_id`.
/// When `project_id` is `Some`, scopes the sequence_id lookup to that project;
/// when `None`, searches across all projects (returns the first match).
pub fn resolve_id(conn: &Connection, project_id: Option<&str>, id_or_seq: &str) -> Result<String> {
    let numeric_part = id_or_seq.strip_prefix('#').unwrap_or(id_or_seq);
    if let Ok(seq) = numeric_part.parse::<i64>() {
        let sql = match project_id {
            Some(_) => {
                "SELECT id FROM items WHERE project_id = ?1 AND sequence_id = ?2 AND deleted_at IS NULL"
            }
            None => "SELECT id FROM items WHERE sequence_id = ?1 AND deleted_at IS NULL LIMIT 1",
        };
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = match project_id {
            Some(pid) => vec![Box::new(pid.to_string()), Box::new(seq)],
            None => vec![Box::new(seq)],
        };
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        conn.query_row(sql, params_ref.as_slice(), |row| row.get(0))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    crate::error::Error::NotFound(format!("sequence_id #{seq}"))
                }
                other => other.into(),
            })
    } else {
        Ok(id_or_seq.to_string())
    }
}

pub fn list_by_project(conn: &Connection, project_id: &str) -> Result<Vec<Item>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, state_id, name, description, priority, parent_id, assignee_agent, sequence_id, sort_order, started_at, completed_at, archived_at, external_source, external_id, metadata, created_at, updated_at, deleted_at
         FROM items WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY sort_order",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id], row_to_item)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn list_by_label(conn: &Connection, project_id: &str, label_id: &str) -> Result<Vec<Item>> {
    let mut stmt = conn.prepare(
        "SELECT items.id, items.project_id, items.state_id, items.name, items.description, items.priority, items.parent_id, items.assignee_agent, items.sequence_id, items.sort_order, items.started_at, items.completed_at, items.archived_at, items.external_source, items.external_id, items.metadata, items.created_at, items.updated_at, items.deleted_at
         FROM items
         INNER JOIN item_labels ON item_labels.item_id = items.id
         WHERE item_labels.label_id = ?1 AND items.project_id = ?2 AND items.deleted_at IS NULL
         ORDER BY items.sort_order",
    )?;
    let rows = stmt.query_map(rusqlite::params![label_id, project_id], row_to_item)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// List non-deleted items assigned to an agent (excludes completed/cancelled).
pub fn list_by_assignee_agent(
    conn: &Connection,
    project_id: &str,
    agent: &str,
) -> Result<Vec<Item>> {
    let mut stmt = conn.prepare(
        "SELECT i.id, i.project_id, i.state_id, i.name, i.description,
                i.priority, i.parent_id, i.assignee_agent, i.sequence_id,
                i.sort_order, i.started_at, i.completed_at, i.archived_at,
                i.external_source, i.external_id, i.metadata,
                i.created_at, i.updated_at, i.deleted_at
         FROM items i
         JOIN states s ON s.id = i.state_id
         WHERE i.project_id = ?1
           AND i.assignee_agent = ?2
           AND i.deleted_at IS NULL
           AND s.group_name NOT IN ('completed', 'cancelled')
         ORDER BY i.sort_order",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id, agent], row_to_item)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn update(conn: &Connection, id: &str, input: UpdateItem) -> Result<Item> {
    let ts = now();
    let assignee_agent = input
        .assignee_agent
        .as_deref()
        .map(agent_registry::canonicalize);
    // Snapshot the outgoing assignee before the write so the assignment log
    // can record the transition (only fetched when the assignee is changing).
    let previous_assignee = if assignee_agent.is_some() {
        Some(get(conn, id)?.assignee_agent)
    } else {
        None
    };
    let mut sets = vec!["updated_at = ?2".to_string()];
    let mut param_idx = 3;
    if input.name.is_some() {
        sets.push(format!("name = ?{param_idx}"));
        param_idx += 1;
    }
    if input.description.is_some() {
        sets.push(format!("description = ?{param_idx}"));
        param_idx += 1;
    }
    if input.priority.is_some() {
        sets.push(format!("priority = ?{param_idx}"));
        param_idx += 1;
    }
    if input.state_id.is_some() {
        sets.push(format!("state_id = ?{param_idx}"));
        param_idx += 1;
    }
    if assignee_agent.is_some() {
        sets.push(format!("assignee_agent = ?{param_idx}"));
        param_idx += 1;
    }
    if input.sort_order.is_some() {
        sets.push(format!("sort_order = ?{param_idx}"));
        param_idx += 1;
    }
    if input.metadata.is_some() {
        sets.push(format!("metadata = ?{param_idx}"));
    }
    let sql = format!(
        "UPDATE items SET {} WHERE id = ?1 AND deleted_at IS NULL",
        sets.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(id.to_string()));
    param_values.push(Box::new(ts));
    if let Some(ref name) = input.name {
        param_values.push(Box::new(name.clone()));
    }
    if let Some(ref desc) = input.description {
        param_values.push(Box::new(desc.clone()));
    }
    if let Some(ref pri) = input.priority {
        param_values.push(Box::new(pri.clone()));
    }
    if let Some(ref sid) = input.state_id {
        param_values.push(Box::new(sid.clone()));
    }
    if let Some(ref agent) = assignee_agent {
        param_values.push(Box::new(agent.clone()));
    }
    if let Some(so) = input.sort_order {
        param_values.push(Box::new(so));
    }
    if let Some(ref metadata) = input.metadata {
        param_values.push(Box::new(metadata.clone()));
    }
    let changed = stmt.execute(rusqlite::params_from_iter(param_values.iter()))?;
    if changed == 0 {
        return Err(crate::error::Error::NotFound(id.to_string()));
    }
    if let (Some(new_assignee), Some(old_assignee)) = (&assignee_agent, &previous_assignee)
        && old_assignee.as_deref() != Some(new_assignee.as_str())
    {
        crate::assignment_events::record(conn, id, old_assignee.as_deref(), new_assignee)?;
    }
    let item = get(conn, id)?;
    if let Ok(wid) = workspace_id_for_project(conn, &item.project_id) {
        events::emit(
            conn,
            &wid,
            "item",
            "update",
            serde_json::to_value(&item).unwrap_or_default(),
        );
    }
    Ok(item)
}

/// Moves an item to a different state within its project. Unlike `update()`,
/// this sets `started_at`/`completed_at` based on the *target* state's
/// group — deliberately not a transition state-machine (Plane itself allows
/// any state → any state; only timestamps follow group membership), so the
/// one real constraint enforced here is that `state_id` belongs to the same
/// project as the item.
pub fn update_state(conn: &Connection, id: &str, state_id: &str) -> Result<Item> {
    let item = get(conn, id)?;
    let state = crate::state::get(conn, state_id)?;
    if state.project_id != item.project_id {
        return Err(crate::error::Error::InvalidTransition(format!(
            "state {state_id} belongs to a different project than item {id}"
        )));
    }
    let ts = now();
    let changed = match state.group_name.as_str() {
        "started" => conn.execute(
            "UPDATE items SET state_id = ?2, started_at = ?3, updated_at = ?3 WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id, state_id, ts],
        )?,
        "completed" => conn.execute(
            "UPDATE items SET state_id = ?2, completed_at = ?3, updated_at = ?3 WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id, state_id, ts],
        )?,
        _ => conn.execute(
            "UPDATE items SET state_id = ?2, updated_at = ?3 WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id, state_id, ts],
        )?,
    };
    if changed == 0 {
        return Err(crate::error::Error::NotFound(id.to_string()));
    }
    let item = get(conn, id)?;
    if let Ok(wid) = workspace_id_for_project(conn, &item.project_id) {
        events::emit(
            conn,
            &wid,
            "item",
            "update",
            serde_json::to_value(&item).unwrap_or_default(),
        );
    }
    Ok(item)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let item = get(conn, id)?;
    let ts = now();
    let changed = conn.execute(
        "UPDATE items SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
        rusqlite::params![ts, id],
    )?;
    if changed == 0 {
        return Err(crate::error::Error::NotFound(id.to_string()));
    }
    if let Ok(wid) = workspace_id_for_project(conn, &item.project_id) {
        events::emit(
            conn,
            &wid,
            "item",
            "delete",
            serde_json::json!({"id": item.id}),
        );
    }
    Ok(())
}

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

/// Outcome of a claim attempt — the raw lease `Acquire` plus the handoff
/// freeze rule: while an item carries an `assignee_agent` that nobody has
/// claimed yet (a handoff sitting unaccepted), only that assignee may
/// acquire it. Once any claim has ever been taken (even a since-stale one),
/// the ordinary `Acquired`/`Held` staleness rules take back over — this
/// variant only covers the fresh, never-claimed window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Acquired,
    Held { owner: String, age_secs: i64 },
    BlockedByAssignee { assignee: String },
}

/// Canonical agent identity of an owner id (`<agent>:<instance>` ->
/// canonical `<agent>`), matching `assignee_agent`'s canonical form —
/// `assignee_agent` is canonicalized on write (see `create`/`update`), but
/// `owner` is the raw caller-supplied id, so an alias like `claude:1` must
/// be canonicalized here too or it won't match `claude-code`.
///
/// `pub` because `assignee_agent` legitimately carries the instance suffix
/// after a claim (`claim()` below stores the raw `owner`, on purpose — see
/// its own doc comment and the tests pinning that), so any caller outside
/// this module that reads `assignee_agent` back to resolve *which agent
/// type* it names (not which specific instance) needs the same stripping
/// this module already does internally, instead of re-deriving it.
pub fn agent_part(owner: &str) -> String {
    agent_registry::canonicalize(owner.split(':').next().unwrap_or(owner))
}

/// Claims an item so other agents don't duplicate the work: on a fresh
/// acquire, sets the assignee and moves state into the project's "started"
/// group (which sets `started_at`, via `update_state`). A live claim held by
/// someone else returns `Held` and leaves the item untouched. An item
/// freshly handed off (assignee set, never yet claimed) to a *different*
/// agent than the caller returns `BlockedByAssignee` instead of letting the
/// caller silently steal it. Acquisition, the state transition, and the
/// assignee update are one transaction — a mid-sequence failure can't leave
/// `item_claims` saying "claimed" while the item itself never reflects it.
pub fn claim(
    conn: &Connection,
    item_id: &str,
    owner: &str,
    now: i64,
    ttl_secs: i64,
) -> Result<ClaimOutcome> {
    let tx = conn.unchecked_transaction()?;
    let item = get(&tx, item_id)?;
    if let Some(assignee) = &item.assignee_agent
        && agent_part(assignee) != agent_part(owner)
        && crate::claim::current_owner(&tx, item_id).is_none()
    {
        // Excludes completed/cancelled items: a done-and-released item is
        // fair game for anyone to re-claim (e.g. reopened follow-up work) —
        // the freeze only protects a handoff that's still open.
        let state = crate::state::get(&tx, &item.state_id)?;
        if !matches!(state.group_name.as_str(), "completed" | "cancelled") {
            return Ok(ClaimOutcome::BlockedByAssignee {
                assignee: assignee.clone(),
            });
        }
    }
    let outcome = crate::claim::acquire(&tx, item_id, owner, now, ttl_secs)?;
    let result = match outcome {
        crate::claim::Acquire::Acquired => {
            let started_state = crate::state::first_in_group(&tx, &item.project_id, "started")?;
            update_state(&tx, item_id, &started_state.id)?;
            update(
                &tx,
                item_id,
                UpdateItem {
                    assignee_agent: Some(owner.to_string()),
                    ..Default::default()
                },
            )?;
            ClaimOutcome::Acquired
        }
        crate::claim::Acquire::Held { owner, age_secs } => ClaimOutcome::Held { owner, age_secs },
    };
    tx.commit()?;
    Ok(result)
}

/// Moves a claimed item into the project's "completed" group WITHOUT
/// releasing the claim lease yet. Deliberately split from the lease release
/// (contrast with the old `claim_done`, which did both atomically): the
/// `"done"` MCP arm calls this, then runs `worktree::push_and_open_pr`
/// (which needs the lease to still look held so a concurrent `claim()` on
/// the same item between mark_completed and the deferred release below is
/// still correctly rejected), and only *after* publish releases the lease
/// via `claim::done`. Returns `Ok(true)` when the item was actually moved
/// to completed, `Ok(false)` when the caller doesn't own the claim.
pub fn mark_completed(conn: &Connection, item_id: &str, owner: &str) -> Result<bool> {
    // One transaction start to finish so the ownership check can't go stale
    // between the guard and the write — without this, a concurrent
    // release()+claim() by a different owner could slip in between the
    // check and update_state below, completing the item out from under its
    // new owner.
    let tx = conn.unchecked_transaction()?;
    if !crate::claim::is_owner(&tx, item_id, owner)? {
        return Ok(false);
    }
    let item = get(&tx, item_id)?;
    let completed_state = crate::state::first_in_group(&tx, &item.project_id, "completed")?;
    update_state(&tx, item_id, &completed_state.id)?;
    tx.commit()?;
    // Keep the claim lease held for the MCP caller's deferred release.
    Ok(true)
}

/// Moves a claimed item into the project's "in_review" group, same shape and
/// same claim-lease-stays-held contract as `mark_completed` above — used
/// instead of it when `done` results in an open PR (item #420). The work
/// isn't actually finished until that PR merges: landing straight on
/// "completed" would show the item as done while its PR is still red or
/// under review, which is the state-side half of the bug `mark_completed`
/// alone had (the other half was deleting the worktree too, fixed in
/// `mcp_server::item::item_done` by only cleaning up when no PR resulted).
///
/// Auto-creates the "in_review" state on first use per project: it's in
/// `state::DEFAULT_STATES` for every project seeded after item #420, but
/// existing projects were seeded before it existed and have no such state
/// to find.
pub fn mark_in_review(conn: &Connection, item_id: &str, owner: &str) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    if !crate::claim::is_owner(&tx, item_id, owner)? {
        return Ok(false);
    }
    let item = get(&tx, item_id)?;
    let review_state = match crate::state::first_in_group(&tx, &item.project_id, "in_review") {
        Ok(s) => s,
        Err(crate::error::Error::NotFound(_)) => crate::state::create(
            &tx,
            crate::state::CreateState {
                project_id: item.project_id.clone(),
                name: crate::state::IN_REVIEW_STATE_NAME.into(),
                group_name: "in_review".into(),
                sequence: crate::state::IN_REVIEW_STATE_SEQUENCE,
                is_default: None,
                color: Some(crate::state::IN_REVIEW_STATE_COLOR.into()),
            },
        )?,
        Err(e) => return Err(e),
    };
    update_state(&tx, item_id, &review_state.id)?;
    tx.commit()?;
    Ok(true)
}

/// Promotes an item from "in_review" to "completed" once its PR is
/// confirmed merged (`item_check_merge`, item #420). Unlike
/// `mark_completed`/`mark_in_review`, this is deliberately NOT owner-scoped:
/// `item_done` leaves the claim lease held (not released) when it moves an
/// item into "in_review" specifically so nobody else can claim it out from
/// under the pending review, so by the time anything reaches this function
/// no other owner could legally exist — whoever notices the merge and calls
/// `check_merge`, possibly a different session than the one that opened the
/// PR, is allowed to finish the transition. Releases whatever lease is
/// still held as part of the same commit. Returns `Ok(false)` (a no-op,
/// not an error) when the item isn't currently in "in_review" — callers
/// can call this speculatively without checking state first.
pub fn promote_in_review_to_completed(conn: &Connection, item_id: &str) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let item = get(&tx, item_id)?;
    let state = crate::state::get(&tx, &item.state_id)?;
    if state.group_name != "in_review" {
        return Ok(false);
    }
    let completed_state = crate::state::first_in_group(&tx, &item.project_id, "completed")?;
    update_state(&tx, item_id, &completed_state.id)?;
    if let Some(owner) = crate::claim::current_owner(&tx, item_id) {
        crate::claim::done(&tx, item_id, &owner, now())?;
    }
    tx.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests;
