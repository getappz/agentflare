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
    let id = uuid::Uuid::now_v7().to_string();
    let ts = now();
    let sort_order = input.sort_order.unwrap_or(65535.0);
    let description = input.description.unwrap_or_default();
    let priority = input.priority.unwrap_or_else(|| "none".to_string());
    let metadata = input.metadata.unwrap_or_else(|| "{}".to_string());

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
            input.assignee_agent,
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

pub fn list_by_project(conn: &Connection, project_id: &str) -> Result<Vec<Item>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, state_id, name, description, priority, parent_id, assignee_agent, sequence_id, sort_order, started_at, completed_at, archived_at, external_source, external_id, metadata, created_at, updated_at, deleted_at
         FROM items WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY sort_order",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id], row_to_item)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn update(conn: &Connection, id: &str, input: UpdateItem) -> Result<Item> {
    let ts = now();
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
    if input.assignee_agent.is_some() {
        sets.push(format!("assignee_agent = ?{param_idx}"));
        param_idx += 1;
    }
    if input.sort_order.is_some() {
        sets.push(format!("sort_order = ?{param_idx}"));
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
    if let Some(ref agent) = input.assignee_agent {
        param_values.push(Box::new(agent.clone()));
    }
    if let Some(so) = input.sort_order {
        param_values.push(Box::new(so));
    }
    let changed = stmt.execute(rusqlite::params_from_iter(param_values.iter()))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::project::{self, CreateProject};
    use crate::workspace::{self, CreateWorkspace};

    fn seed_project(conn: &Connection, suffix: &str) -> (String, String) {
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
                workspace_id: ws.id.clone(),
                name: format!("Test{suffix}"),
                identifier: format!("T{suffix}"),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let states = crate::state::list_by_project(conn, &proj.id).unwrap();
        let state_id = states
            .iter()
            .find(|s| s.is_default)
            .map(|s| s.id.clone())
            .unwrap();
        (proj.id, state_id)
    }

    #[test]
    fn create_and_get() {
        let conn = db::open_in_memory().unwrap();
        let (pid, sid) = seed_project(&conn, "");
        let item = create(
            &conn,
            CreateItem {
                project_id: pid,
                state_id: sid,
                name: "Test Item".into(),
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
            },
        )
        .unwrap();
        assert_eq!(item.name, "Test Item");
        assert_eq!(item.sequence_id, 1);
        let got = get(&conn, &item.id).unwrap();
        assert_eq!(got.id, item.id);
    }

    #[test]
    fn sequence_increments() {
        let conn = db::open_in_memory().unwrap();
        let (pid, sid) = seed_project(&conn, "");
        let i1 = create(
            &conn,
            CreateItem {
                project_id: pid.clone(),
                state_id: sid.clone(),
                name: "First".into(),
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
            },
        )
        .unwrap();
        let i2 = create(
            &conn,
            CreateItem {
                project_id: pid,
                state_id: sid,
                name: "Second".into(),
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
            },
        )
        .unwrap();
        assert_eq!(i1.sequence_id, 1);
        assert_eq!(i2.sequence_id, 2);
    }

    #[test]
    fn list_by_project_scopes() {
        let conn = db::open_in_memory().unwrap();
        let (pid1, sid1) = seed_project(&conn, "1");
        let (pid2, _sid2) = seed_project(&conn, "2");
        create(
            &conn,
            CreateItem {
                project_id: pid1.clone(),
                state_id: sid1,
                name: "Item 1".into(),
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
            },
        )
        .unwrap();
        assert_eq!(list_by_project(&conn, &pid1).unwrap().len(), 1);
        assert_eq!(list_by_project(&conn, &pid2).unwrap().len(), 0);
    }

    #[test]
    fn add_and_remove_labels() {
        let conn = db::open_in_memory().unwrap();
        let (pid, sid) = seed_project(&conn, "");
        let item = create(
            &conn,
            CreateItem {
                project_id: pid.clone(),
                state_id: sid,
                name: "Test".into(),
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
            },
        )
        .unwrap();
        let ws = crate::workspace::list(&conn)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let label = crate::label::create(
            &conn,
            crate::label::CreateLabel {
                project_id: Some(pid),
                workspace_id: ws.id,
                name: "bug".into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        add_label(&conn, &item.id, &label.id).unwrap();
        let labels = list_labels(&conn, &item.id).unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0], label.id);
        remove_label(&conn, &item.id, &label.id).unwrap();
        assert!(list_labels(&conn, &item.id).unwrap().is_empty());
    }

    #[test]
    fn add_and_remove_assignees() {
        let conn = db::open_in_memory().unwrap();
        let (pid, sid) = seed_project(&conn, "");
        let item = create(
            &conn,
            CreateItem {
                project_id: pid,
                state_id: sid,
                name: "Test".into(),
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
            },
        )
        .unwrap();
        add_assignee(&conn, &item.id, "agent:1").unwrap();
        add_assignee(&conn, &item.id, "agent:2").unwrap();
        let agents = list_assignees(&conn, &item.id).unwrap();
        assert_eq!(agents.len(), 2);
        remove_assignee(&conn, &item.id, "agent:1").unwrap();
        assert_eq!(list_assignees(&conn, &item.id).unwrap().len(), 1);
    }

    #[test]
    fn add_and_remove_dependencies() {
        let conn = db::open_in_memory().unwrap();
        let (pid, sid) = seed_project(&conn, "");
        let i1 = create(
            &conn,
            CreateItem {
                project_id: pid.clone(),
                state_id: sid.clone(),
                name: "A".into(),
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
            },
        )
        .unwrap();
        let i2 = create(
            &conn,
            CreateItem {
                project_id: pid,
                state_id: sid,
                name: "B".into(),
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
            },
        )
        .unwrap();
        let i1_id = i1.id.clone();
        let i2_id = i2.id.clone();
        add_dependency(&conn, &i2_id, &i1_id).unwrap();
        let deps = list_dependencies(&conn, &i2_id).unwrap();
        assert_eq!(deps, vec![i1_id.clone()]);
        remove_dependency(&conn, &i2_id, &i1_id).unwrap();
        assert!(list_dependencies(&conn, &i2.id).unwrap().is_empty());
    }

    #[test]
    fn create_wires_up_label_assignee_and_dependency_ids() {
        // Regression test: CreateItem.label_ids/assignee_ids/dependency_ids
        // must actually be attached by create(), not silently dropped.
        let conn = db::open_in_memory().unwrap();
        let (pid, sid) = seed_project(&conn, "");
        let ws = crate::workspace::list(&conn)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let label = crate::label::create(
            &conn,
            crate::label::CreateLabel {
                project_id: Some(pid.clone()),
                workspace_id: ws.id,
                name: "bug".into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let blocker = create(
            &conn,
            CreateItem {
                project_id: pid.clone(),
                state_id: sid.clone(),
                name: "Blocker".into(),
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
            },
        )
        .unwrap();
        let item = create(
            &conn,
            CreateItem {
                project_id: pid,
                state_id: sid,
                name: "Test".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![label.id.clone()],
                assignee_ids: vec!["agent:1".into()],
                dependency_ids: vec![blocker.id.clone()],
            },
        )
        .unwrap();
        assert_eq!(list_labels(&conn, &item.id).unwrap(), vec![label.id]);
        assert_eq!(
            list_assignees(&conn, &item.id).unwrap(),
            vec!["agent:1".to_string()]
        );
        assert_eq!(
            list_dependencies(&conn, &item.id).unwrap(),
            vec![blocker.id]
        );
    }

    fn state_in_group(conn: &Connection, project_id: &str, group: &str) -> String {
        crate::state::list_by_project(conn, project_id)
            .unwrap()
            .into_iter()
            .find(|s| s.group_name == group)
            .unwrap()
            .id
    }

    #[test]
    fn update_state_sets_started_at_when_moving_into_started_group() {
        let conn = db::open_in_memory().unwrap();
        let (pid, sid) = seed_project(&conn, "");
        let item = create(
            &conn,
            CreateItem {
                project_id: pid.clone(),
                state_id: sid,
                name: "Test".into(),
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
            },
        )
        .unwrap();
        assert!(item.started_at.is_none());
        let started_state = state_in_group(&conn, &pid, "started");
        let updated = update_state(&conn, &item.id, &started_state).unwrap();
        assert!(updated.started_at.is_some());
        assert!(updated.completed_at.is_none());
    }

    #[test]
    fn update_state_sets_completed_at_when_moving_into_completed_group() {
        let conn = db::open_in_memory().unwrap();
        let (pid, sid) = seed_project(&conn, "");
        let item = create(
            &conn,
            CreateItem {
                project_id: pid.clone(),
                state_id: sid,
                name: "Test".into(),
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
            },
        )
        .unwrap();
        let completed_state = state_in_group(&conn, &pid, "completed");
        let updated = update_state(&conn, &item.id, &completed_state).unwrap();
        assert!(updated.completed_at.is_some());
    }

    #[test]
    fn update_state_leaves_timestamps_none_when_moving_into_backlog() {
        let conn = db::open_in_memory().unwrap();
        let (pid, sid) = seed_project(&conn, "");
        let item = create(
            &conn,
            CreateItem {
                project_id: pid.clone(),
                state_id: sid,
                name: "Test".into(),
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
            },
        )
        .unwrap();
        let backlog_state = state_in_group(&conn, &pid, "backlog");
        let updated = update_state(&conn, &item.id, &backlog_state).unwrap();
        assert!(updated.started_at.is_none());
        assert!(updated.completed_at.is_none());
    }

    #[test]
    fn update_state_rejects_state_from_a_different_project() {
        let conn = db::open_in_memory().unwrap();
        let (pid1, sid1) = seed_project(&conn, "1");
        let (pid2, _sid2) = seed_project(&conn, "2");
        let item = create(
            &conn,
            CreateItem {
                project_id: pid1,
                state_id: sid1,
                name: "Test".into(),
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
            },
        )
        .unwrap();
        let other_project_state = state_in_group(&conn, &pid2, "started");
        assert!(matches!(
            update_state(&conn, &item.id, &other_project_state),
            Err(crate::error::Error::InvalidTransition(_))
        ));
    }
}
