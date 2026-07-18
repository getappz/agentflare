use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::events;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub group_name: String,
    pub sequence: f64,
    pub is_default: bool,
    pub color: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateState {
    pub project_id: String,
    pub name: String,
    pub group_name: String,
    pub sequence: f64,
    pub is_default: Option<bool>,
    pub color: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateState {
    pub name: Option<String>,
    pub sequence: Option<f64>,
    pub color: Option<String>,
}

const DEFAULT_STATES: &[(&str, &str, f64, &str)] = &[
    ("Backlog", "backlog", 15000.0, "#60646C"),
    ("Todo", "unstarted", 25000.0, "#60646C"),
    ("In Progress", "started", 35000.0, "#F59E0B"),
    ("Done", "completed", 45000.0, "#46A758"),
    ("Cancelled", "cancelled", 55000.0, "#9AA4BC"),
    ("Triage", "triage", 65000.0, "#4E5355"),
];

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

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_to_state(row: &rusqlite::Row) -> rusqlite::Result<State> {
    Ok(State {
        id: row.get(0)?,
        project_id: row.get(1)?,
        name: row.get(2)?,
        group_name: row.get(3)?,
        sequence: row.get(4)?,
        is_default: row.get::<_, i64>(5)? != 0,
        color: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        deleted_at: row.get(9)?,
    })
}

pub fn seed_defaults(conn: &Connection, project_id: &str) -> Result<()> {
    let ts = now();
    for (i, (name, group, seq, color)) in DEFAULT_STATES.iter().enumerate() {
        let id = db_kit::ids::new_id();
        let is_default = if i == 0 { 1 } else { 0 };
        conn.execute(
            "INSERT INTO states (id, project_id, name, group_name, sequence, is_default, color, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![id, project_id, name, group, seq, is_default, color, ts, ts],
        )?;
    }
    Ok(())
}

pub fn create(conn: &Connection, input: CreateState) -> Result<State> {
    let id = db_kit::ids::new_id();
    let ts = now();
    let is_default = if input.is_default.unwrap_or(false) {
        1
    } else {
        0
    };
    let color = input.color.unwrap_or_else(|| "#60646C".to_string());
    conn.execute(
        "INSERT INTO states (id, project_id, name, group_name, sequence, is_default, color, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![id, input.project_id, input.name, input.group_name, input.sequence, is_default, color, ts, ts],
    )?;
    let state = get(conn, &id)?;
    if let Ok(wid) = workspace_id_for_project(conn, &input.project_id) {
        events::emit(
            conn,
            &wid,
            "state",
            "create",
            serde_json::to_value(&state).unwrap_or_default(),
        );
    }
    Ok(state)
}

pub fn get(conn: &Connection, id: &str) -> Result<State> {
    conn.query_row(
        "SELECT id, project_id, name, group_name, sequence, is_default, color, created_at, updated_at, deleted_at
         FROM states WHERE id = ?1 AND deleted_at IS NULL",
        rusqlite::params![id],
        row_to_state,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => crate::error::Error::NotFound(id.to_string()),
        other => other.into(),
    })
}

/// First state (by sequence) in `group` for the project — used to resolve
/// the "Started"/"Completed" target when claiming or completing an item.
pub fn first_in_group(conn: &Connection, project_id: &str, group: &str) -> Result<State> {
    conn.query_row(
        "SELECT id, project_id, name, group_name, sequence, is_default, color, created_at, updated_at, deleted_at
         FROM states WHERE project_id = ?1 AND group_name = ?2 AND deleted_at IS NULL ORDER BY sequence LIMIT 1",
        rusqlite::params![project_id, group],
        row_to_state,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
            crate::error::Error::NotFound(format!("no '{group}' state for project {project_id}"))
        }
        other => other.into(),
    })
}

pub fn list_by_project(conn: &Connection, project_id: &str) -> Result<Vec<State>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, name, group_name, sequence, is_default, color, created_at, updated_at, deleted_at
         FROM states WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY sequence",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id], row_to_state)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn update(conn: &Connection, id: &str, input: UpdateState) -> Result<State> {
    let ts = now();
    let mut sets = vec!["updated_at = ?2".to_string()];
    let mut param_idx = 3;
    if input.name.is_some() {
        sets.push(format!("name = ?{param_idx}"));
        param_idx += 1;
    }
    if input.sequence.is_some() {
        sets.push(format!("sequence = ?{param_idx}"));
        param_idx += 1;
    }
    if input.color.is_some() {
        sets.push(format!("color = ?{param_idx}"));
    }
    let sql = format!(
        "UPDATE states SET {} WHERE id = ?1 AND deleted_at IS NULL",
        sets.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(id.to_string()));
    param_values.push(Box::new(ts));
    if let Some(ref name) = input.name {
        param_values.push(Box::new(name.clone()));
    }
    if let Some(seq) = input.sequence {
        param_values.push(Box::new(seq));
    }
    if let Some(ref color) = input.color {
        param_values.push(Box::new(color.clone()));
    }
    let changed = stmt.execute(rusqlite::params_from_iter(param_values.iter()))?;
    if changed == 0 {
        return Err(crate::error::Error::NotFound(id.to_string()));
    }
    let state = get(conn, id)?;
    if let Ok(wid) = workspace_id_for_project(conn, &state.project_id) {
        events::emit(
            conn,
            &wid,
            "state",
            "update",
            serde_json::to_value(&state).unwrap_or_default(),
        );
    }
    Ok(state)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let state = get(conn, id)?;
    if state.is_default {
        return Err(crate::error::Error::InvalidTransition(
            "cannot delete the project's default state; items need a state to fall back to".into(),
        ));
    }
    let ts = now();
    let changed = conn.execute(
        "UPDATE states SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
        rusqlite::params![ts, id],
    )?;
    if changed == 0 {
        return Err(crate::error::Error::NotFound(id.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::project::{self, CreateProject};
    use crate::workspace::{self, CreateWorkspace};

    fn seed_project(conn: &Connection) -> String {
        let ws = workspace::create(
            conn,
            CreateWorkspace {
                name: "Test".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = project::create(
            conn,
            CreateProject {
                workspace_id: ws.id.clone(),
                name: "Test Project".into(),
                identifier: "TEST".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        proj.id
    }

    #[test]
    fn seed_defaults_creates_six_states() {
        let conn = db::open_in_memory().unwrap();
        let pid = seed_project(&conn);
        let states = list_by_project(&conn, &pid).unwrap();
        assert_eq!(states.len(), 6);
        assert!(states.iter().any(|s| s.name == "Backlog" && s.is_default));
        assert!(
            states
                .iter()
                .any(|s| s.name == "Done" && s.group_name == "completed")
        );
    }

    #[test]
    fn first_in_group_returns_lowest_sequence_match() {
        let conn = db::open_in_memory().unwrap();
        let pid = seed_project(&conn);
        let started = first_in_group(&conn, &pid, "started").unwrap();
        assert_eq!(started.name, "In Progress");
        assert_eq!(started.group_name, "started");
    }

    #[test]
    fn first_in_group_errors_when_no_state_matches() {
        let conn = db::open_in_memory().unwrap();
        let pid = seed_project(&conn);
        assert!(matches!(
            first_in_group(&conn, &pid, "no-such-group"),
            Err(crate::error::Error::NotFound(_))
        ));
    }

    #[test]
    fn create_custom_state() {
        let conn = db::open_in_memory().unwrap();
        let pid = seed_project(&conn);
        let s = create(
            &conn,
            CreateState {
                project_id: pid.clone(),
                name: "Under Review".into(),
                group_name: "started".into(),
                sequence: 40000.0,
                is_default: None,
                color: None,
            },
        )
        .unwrap();
        assert_eq!(s.name, "Under Review");
        assert_eq!(s.group_name, "started");
    }

    #[test]
    fn list_by_project_scopes() {
        let conn = db::open_in_memory().unwrap();
        let pid1 = seed_project(&conn);
        let pid2 = {
            let ws = workspace::create(
                &conn,
                CreateWorkspace {
                    name: "Other WS".into(),
                    slug: "other-ws".into(),
                    owner_agent: None,
                    item_label: None,
                },
            )
            .unwrap();
            project::create(
                &conn,
                CreateProject {
                    workspace_id: ws.id,
                    name: "Other Proj".into(),
                    identifier: "OTHER".into(),
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap()
            .id
        };
        assert_eq!(list_by_project(&conn, &pid1).unwrap().len(), 6);
        assert_eq!(list_by_project(&conn, &pid2).unwrap().len(), 6);
    }

    #[test]
    fn delete_soft() {
        let conn = db::open_in_memory().unwrap();
        let pid = seed_project(&conn);
        let states = list_by_project(&conn, &pid).unwrap();
        let sid = states.iter().find(|s| !s.is_default).unwrap().id.clone();
        delete(&conn, &sid).unwrap();
        assert!(matches!(
            get(&conn, &sid),
            Err(crate::error::Error::NotFound(_))
        ));
    }

    #[test]
    fn delete_rejects_the_default_state() {
        let conn = db::open_in_memory().unwrap();
        let pid = seed_project(&conn);
        let states = list_by_project(&conn, &pid).unwrap();
        let default_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        assert!(matches!(
            delete(&conn, &default_id),
            Err(crate::error::Error::InvalidTransition(_))
        ));
        assert!(get(&conn, &default_id).is_ok());
    }
}
