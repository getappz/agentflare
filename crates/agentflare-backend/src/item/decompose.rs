//! Turns an approved plan into child items of an epic. Idempotent: children
//! are keyed by (`plan:<epic id>`, `task-<N>`), created if missing, never
//! rewritten. Each child is one `create` transaction, so a crash leaves only
//! whole children and a rerun fills the gap.
//!
//! Scheduling needs nothing from here: root children get `ready-for-work`;
//! `supervisor::cascade_unblock_dependents` labels the rest as their blockers
//! complete, and `quota::decide` Tier 4b holds back anything still blocked.

use std::collections::BTreeMap;

use rusqlite::Connection;

use super::plan_format::{ParsedPlan, PlanTask, parse_plan};
use super::{CreateItem, create, get, update_state};
use crate::error::{Error, Result};

pub const READY_LABEL: &str = "ready-for-work";

pub fn plan_source(epic_id: &str) -> String {
    format!("plan:{epic_id}")
}

#[derive(Debug, PartialEq, Eq)]
pub struct DecomposeOutcome {
    pub created: usize,
    pub existing: usize,
    /// Problems the caller should surface (e.g. as an epic comment) that do
    /// not stop decomposition.
    pub warnings: Vec<String>,
}

pub fn decompose_plan(
    conn: &Connection,
    epic_id: &str,
    plan_markdown: &str,
) -> Result<DecomposeOutcome> {
    let plan = parse_plan(plan_markdown).map_err(|errs| Error::Validation(errs.join("; ")))?;
    let epic = get(conn, epic_id)?;
    let source = plan_source(epic_id);
    let ready = crate::label::ensure_project_label(conn, &epic.project_id, READY_LABEL)?;
    let state = crate::state::list_by_project(conn, &epic.project_id)?
        .into_iter()
        .find(|s| s.is_default)
        .ok_or_else(|| Error::NotFound(format!("default state for project {}", epic.project_id)))?;

    let mut ids: BTreeMap<usize, String> = BTreeMap::new();
    let (mut created, mut existing) = (0, 0);
    let mut warnings = Vec::new();
    let mut pending: Vec<&PlanTask> = plan.tasks.iter().collect();
    while !pending.is_empty() {
        let (now, later): (Vec<&PlanTask>, Vec<&PlanTask>) = pending
            .into_iter()
            .partition(|t| t.depends_on.iter().all(|d| ids.contains_key(d)));
        if now.is_empty() {
            return Err(Error::Validation("dependency cycle in plan".to_string()));
        }
        for task in now {
            let external_id = format!("task-{}", task.no);
            if let Some(id) = find_child(conn, &source, &external_id)? {
                ids.insert(task.no, id);
                existing += 1;
                continue;
            }
            // Existing dependents keep pointing at a deleted child's old id and
            // are never rewritten, so they would wait on it forever. Say so.
            if let Some(old) = find_deleted_child(conn, &source, &external_id)? {
                let stranded = super::dependents_of(conn, &old)?.len();
                if stranded > 0 {
                    warnings.push(format!(
                        "task {} was deleted and recreated; {stranded} existing child(ren) still depend on the deleted item {old} and will never unblock",
                        task.no
                    ));
                }
            }
            let item = create(
                conn,
                CreateItem {
                    project_id: epic.project_id.clone(),
                    state_id: state.id.clone(),
                    name: format!("Task {}: {}", task.no, task.title),
                    description: Some(describe(&plan, task)),
                    priority: Some(epic.priority.clone()),
                    parent_id: Some(epic.id.clone()),
                    assignee_agent: epic.assignee_agent.clone(),
                    sort_order: None,
                    external_source: Some(source.clone()),
                    external_id: Some(external_id),
                    metadata: Some(metadata_json(task)),
                    // Label now if every blocker is already completed (true for
                    // roots too): the dependency cascade only fires when a
                    // blocker completes, so it would never label this child.
                    label_ids: if deps_completed(
                        conn,
                        task.depends_on.iter().map(|d| ids[d].as_str()),
                    )? {
                        vec![ready.id.clone()]
                    } else {
                        vec![]
                    },
                    assignee_ids: vec![],
                    dependency_ids: task.depends_on.iter().map(|d| ids[d].clone()).collect(),
                    start_date: None,
                    due_date: None,
                },
            )?;
            ids.insert(task.no, item.id);
            created += 1;
        }
        pending = later;
    }
    if ids.len() != plan.tasks.len() {
        return Err(Error::Validation(format!(
            "decompose produced {} children for {} tasks",
            ids.len(),
            plan.tasks.len()
        )));
    }

    if epic.assignee_agent.is_none() {
        warnings.push(
            "epic has no assignee_agent: root children will not auto-dispatch until one is set"
                .to_string(),
        );
    }
    Ok(DecomposeOutcome {
        created,
        existing,
        warnings,
    })
}

/// Moves `epic_id` to its project's completed state once it has plan children
/// and every one is completed or cancelled. Returns true only if the epic was
/// actually closed (`completed_at` set): `item done` has silently no-opped
/// before, so callers get an honest signal instead of assuming success.
pub fn close_epic_if_children_done(conn: &Connection, epic_id: &str) -> Result<bool> {
    let (total, open): (i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN s.group_name NOT IN ('completed', 'cancelled') THEN 1 ELSE 0 END), 0)
         FROM items i JOIN states s ON s.id = i.state_id
         WHERE i.parent_id = ?1 AND i.external_source = ?2 AND i.deleted_at IS NULL",
        rusqlite::params![epic_id, plan_source(epic_id)],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if total == 0 || open > 0 {
        return Ok(false);
    }
    let epic = get(conn, epic_id)?;
    // Already finished (completed in any completed-group state, or cancelled):
    // moving it to the first completed state would overwrite `completed_at`
    // or turn a cancellation into a completion.
    let group = crate::state::get(conn, &epic.state_id)?.group_name;
    if matches!(group.as_str(), "completed" | "cancelled") {
        return Ok(false);
    }
    let done = crate::state::first_in_group(conn, &epic.project_id, "completed")?;
    Ok(update_state(conn, epic_id, &done.id)?
        .completed_at
        .is_some())
}

/// True when every dependency is in the `completed` group (vacuously true for none).
fn deps_completed<'a>(conn: &Connection, deps: impl Iterator<Item = &'a str>) -> Result<bool> {
    for id in deps {
        let item = get(conn, id)?;
        if crate::state::get(conn, &item.state_id)?.group_name != "completed" {
            return Ok(false);
        }
    }
    Ok(true)
}

fn find_deleted_child(
    conn: &Connection,
    source: &str,
    external_id: &str,
) -> Result<Option<String>> {
    match conn.query_row(
        "SELECT id FROM items WHERE external_source = ?1 AND external_id = ?2 AND deleted_at IS NOT NULL ORDER BY deleted_at DESC LIMIT 1",
        rusqlite::params![source, external_id],
        |row| row.get::<_, String>(0),
    ) {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn find_child(conn: &Connection, source: &str, external_id: &str) -> Result<Option<String>> {
    match conn.query_row(
        "SELECT id FROM items WHERE external_source = ?1 AND external_id = ?2 AND deleted_at IS NULL",
        rusqlite::params![source, external_id],
        |row| row.get::<_, String>(0),
    ) {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The dispatched worker reads only the item description, so everything a
/// task needs (the plan's header context and the task body) goes in it.
fn describe(plan: &ParsedPlan, task: &PlanTask) -> String {
    format!(
        "{}\n\n## Task {}: {}\n\n{}",
        plan.header.trim(),
        task.no,
        task.title,
        task.body
    )
}

fn metadata_json(task: &PlanTask) -> String {
    serde_json::json!({
        "size": task.size,
        "model_tier": task.model_tier,
        "files": task.files,
        "task_no": task.no,
        "parallel": task.parallel,
        "plan_required": false,
    })
    .to_string()
}
