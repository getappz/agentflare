use rusqlite::Connection;

use crate::error::Result;
use crate::events;

use super::relations::{add_assignee, add_dependency, add_label};
use super::{
    CreateItem, Item, UpdateItem, next_sequence_id, now, row_to_item, workspace_id_for_project,
};

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
    validate_date_range(input.start_date, input.due_date)?;

    let tx = conn.unchecked_transaction()?;
    let seq = next_sequence_id(&tx, &input.project_id)?;
    tx.execute(
        "INSERT INTO items (id, project_id, state_id, name, description, priority, parent_id, assignee_agent, sequence_id, sort_order, external_source, external_id, metadata, created_at, updated_at, start_date, due_date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
            input.start_date,
            input.due_date,
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
        "SELECT id, project_id, state_id, name, description, priority, parent_id, assignee_agent, sequence_id, sort_order, started_at, completed_at, archived_at, external_source, external_id, metadata, created_at, updated_at, deleted_at, start_date, due_date
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
        "SELECT id, project_id, state_id, name, description, priority, parent_id, assignee_agent, sequence_id, sort_order, started_at, completed_at, archived_at, external_source, external_id, metadata, created_at, updated_at, deleted_at, start_date, due_date
         FROM items WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY sort_order",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id], row_to_item)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn list_by_label(conn: &Connection, project_id: &str, label_id: &str) -> Result<Vec<Item>> {
    let mut stmt = conn.prepare(
        "SELECT items.id, items.project_id, items.state_id, items.name, items.description, items.priority, items.parent_id, items.assignee_agent, items.sequence_id, items.sort_order, items.started_at, items.completed_at, items.archived_at, items.external_source, items.external_id, items.metadata, items.created_at, items.updated_at, items.deleted_at, items.start_date, items.due_date
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
                i.created_at, i.updated_at, i.deleted_at,
                i.start_date, i.due_date
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

/// Reads an item's `parent_id` without the `deleted_at IS NULL` filter `get`
/// applies, and without erroring on a missing row — an ancestor walk must be
/// able to cross a soft-deleted ancestor and simply stop when the chain runs
/// out.
fn parent_of(conn: &Connection, id: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT parent_id FROM items WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// No CHECK constraint enforces this at the schema level (migration 0012
/// added `start_date`/`due_date` as plain nullable columns) — validated here
/// instead so either bound can be set independently without a table rebuild.
fn validate_date_range(start_date: Option<i64>, due_date: Option<i64>) -> Result<()> {
    if let (Some(start), Some(due)) = (start_date, due_date)
        && due < start
    {
        return Err(crate::error::Error::Validation(format!(
            "due_date ({due}) cannot be before start_date ({start})"
        )));
    }
    Ok(())
}

/// Rejects a re-parent that would make `id` its own ancestor. Only `update`
/// can create a cycle — `create` writes a parent onto a brand-new id that
/// nothing can point at yet — and a cycle would hang every ancestor walk in
/// the tree (`quota::goal::find_goal_ancestor`, the dashboard hierarchy, ...).
/// Also proves the parent exists, so an unknown id fails as `NotFound` rather
/// than a raw "FOREIGN KEY constraint failed".
fn validate_parent(conn: &Connection, id: &str, parent_id: &str) -> Result<()> {
    if parent_id == id {
        return Err(crate::error::Error::Validation(format!(
            "item {id} cannot be its own parent"
        )));
    }
    get(conn, parent_id)?;
    let mut cursor = parent_of(conn, parent_id)?;
    // Bounded so a cycle already present in the data (written before this
    // check existed) errors out instead of looping forever.
    for _ in 0..256 {
        let Some(current) = cursor else {
            return Ok(());
        };
        if current == id {
            return Err(crate::error::Error::Validation(format!(
                "item {parent_id} is a descendant of {id} — re-parenting would create a cycle"
            )));
        }
        cursor = parent_of(conn, &current)?;
    }
    Err(crate::error::Error::Validation(format!(
        "parent chain above {parent_id} is deeper than 256 items or already cyclic"
    )))
}

pub fn update(conn: &Connection, id: &str, input: UpdateItem) -> Result<Item> {
    let ts = now();
    if let Some(Some(parent_id)) = input.parent_id.as_ref() {
        validate_parent(conn, id, parent_id)?;
    }
    if input.start_date.is_some() || input.due_date.is_some() {
        let current = get(conn, id)?;
        let effective_start = input.start_date.or(current.start_date);
        let effective_due = input.due_date.or(current.due_date);
        validate_date_range(effective_start, effective_due)?;
    }
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
        param_idx += 1;
    }
    if input.parent_id.is_some() {
        sets.push(format!("parent_id = ?{param_idx}"));
        param_idx += 1;
    }
    if input.start_date.is_some() {
        sets.push(format!("start_date = ?{param_idx}"));
        param_idx += 1;
    }
    if input.due_date.is_some() {
        sets.push(format!("due_date = ?{param_idx}"));
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
    if let Some(ref parent_id) = input.parent_id {
        param_values.push(Box::new(parent_id.clone()));
    }
    if let Some(start_date) = input.start_date {
        param_values.push(Box::new(start_date));
    }
    if let Some(due_date) = input.due_date {
        param_values.push(Box::new(due_date));
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

/// Clears `start_date` back to NULL — direct SQL, not `update()`, mirroring
/// `claim::release`'s clear of `assignee_agent`: `UpdateItem.start_date` is a
/// plain `Option<i64>` where `None` means "leave untouched", so it has no way
/// to express an explicit clear.
pub fn clear_item_start_date(conn: &Connection, id: &str) -> Result<Item> {
    let ts = now();
    let changed = conn.execute(
        "UPDATE items SET start_date = NULL, updated_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
        rusqlite::params![id, ts],
    )?;
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

/// Clears `due_date` back to NULL — same shape as `clear_item_start_date`.
pub fn clear_item_due_date(conn: &Connection, id: &str) -> Result<Item> {
    let ts = now();
    let changed = conn.execute(
        "UPDATE items SET due_date = NULL, updated_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
        rusqlite::params![id, ts],
    )?;
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
