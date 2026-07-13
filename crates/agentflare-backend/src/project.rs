use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::events;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub identifier: String,
    pub archived_at: Option<i64>,
    pub external_source: Option<String>,
    pub external_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProject {
    pub workspace_id: String,
    pub name: String,
    pub identifier: String,
    pub external_source: Option<String>,
    pub external_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateProject {
    pub name: Option<String>,
    pub identifier: Option<String>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_to_project(row: &rusqlite::Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        identifier: row.get(3)?,
        archived_at: row.get(4)?,
        external_source: row.get(5)?,
        external_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        deleted_at: row.get(9)?,
    })
}

pub fn create(conn: &Connection, input: CreateProject) -> Result<Project> {
    let id = uuid::Uuid::now_v7().to_string();
    let ts = now();
    // One transaction: a project must never exist with fewer than its 6
    // seeded default states. Without this, a crash between the INSERT and
    // seed_defaults (each of which auto-commits on its own otherwise) leaves
    // a project row with zero or partial states.
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO projects (id, workspace_id, name, identifier, external_source, external_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![id, input.workspace_id, input.name, input.identifier, input.external_source, input.external_id, ts, ts],
    )?;
    crate::state::seed_defaults(&tx, &id)?;
    tx.commit()?;
    let proj = get(conn, &id)?;
    events::emit(
        conn,
        &proj.workspace_id,
        "project",
        "create",
        serde_json::to_value(&proj).unwrap_or_default(),
    );
    Ok(proj)
}

pub fn get(conn: &Connection, id: &str) -> Result<Project> {
    conn.query_row(
        "SELECT id, workspace_id, name, identifier, archived_at, external_source, external_id, created_at, updated_at, deleted_at
         FROM projects WHERE id = ?1 AND deleted_at IS NULL",
        rusqlite::params![id],
        row_to_project,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => crate::error::Error::NotFound(id.to_string()),
        other => other.into(),
    })
}

pub fn list_by_workspace(conn: &Connection, workspace_id: &str) -> Result<Vec<Project>> {
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, name, identifier, archived_at, external_source, external_id, created_at, updated_at, deleted_at
         FROM projects WHERE workspace_id = ?1 AND deleted_at IS NULL ORDER BY created_at",
    )?;
    let rows = stmt.query_map(rusqlite::params![workspace_id], row_to_project)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn update(conn: &Connection, id: &str, input: UpdateProject) -> Result<Project> {
    let ts = now();
    let mut sets = vec!["updated_at = ?2".to_string()];
    let mut param_idx = 3;
    if input.name.is_some() {
        sets.push(format!("name = ?{param_idx}"));
        param_idx += 1;
    }
    if input.identifier.is_some() {
        sets.push(format!("identifier = ?{param_idx}"));
    }
    let sql = format!(
        "UPDATE projects SET {} WHERE id = ?1 AND deleted_at IS NULL",
        sets.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(id.to_string()));
    param_values.push(Box::new(ts));
    if let Some(ref name) = input.name {
        param_values.push(Box::new(name.clone()));
    }
    if let Some(ref ident) = input.identifier {
        param_values.push(Box::new(ident.clone()));
    }
    let changed = stmt.execute(rusqlite::params_from_iter(param_values.iter()))?;
    if changed == 0 {
        return Err(crate::error::Error::NotFound(id.to_string()));
    }
    let proj = get(conn, id)?;
    events::emit(
        conn,
        &proj.workspace_id,
        "project",
        "update",
        serde_json::to_value(&proj).unwrap_or_default(),
    );
    Ok(proj)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let proj = get(conn, id)?;
    let ts = now();
    let changed = conn.execute(
        "UPDATE projects SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
        rusqlite::params![ts, id],
    )?;
    if changed == 0 {
        return Err(crate::error::Error::NotFound(id.to_string()));
    }
    events::emit(
        conn,
        &proj.workspace_id,
        "project",
        "delete",
        serde_json::json!({"id": proj.id}),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::workspace::{self, CreateWorkspace};

    fn seed_workspace(conn: &Connection) -> String {
        workspace::create(
            conn,
            CreateWorkspace {
                name: "Test".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap()
        .id
    }

    #[test]
    fn create_and_get() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        let proj = create(
            &conn,
            CreateProject {
                workspace_id: wid,
                name: "My Project".into(),
                identifier: "PROJ".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        assert_eq!(proj.name, "My Project");
        assert_eq!(proj.identifier, "PROJ");
        let got = get(&conn, &proj.id).unwrap();
        assert_eq!(got.id, proj.id);
    }

    #[test]
    fn create_seeds_states() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        let proj = create(
            &conn,
            CreateProject {
                workspace_id: wid,
                name: "Test".into(),
                identifier: "T".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let states = crate::state::list_by_project(&conn, &proj.id).unwrap();
        assert_eq!(states.len(), 6);
    }

    #[test]
    fn list_by_workspace_scopes() {
        let conn = db::open_in_memory().unwrap();
        let wid1 = seed_workspace(&conn);
        let wid2 = {
            let ws = workspace::create(
                &conn,
                CreateWorkspace {
                    name: "Other".into(),
                    slug: "other".into(),
                    owner_agent: None,
                    item_label: None,
                },
            )
            .unwrap();
            ws.id
        };
        create(
            &conn,
            CreateProject {
                workspace_id: wid1.clone(),
                name: "P1".into(),
                identifier: "P1".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        create(
            &conn,
            CreateProject {
                workspace_id: wid2,
                name: "P2".into(),
                identifier: "P2".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let list1 = list_by_workspace(&conn, &wid1).unwrap();
        assert_eq!(list1.len(), 1);
    }

    #[test]
    fn duplicate_name_in_workspace_fails() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        create(
            &conn,
            CreateProject {
                workspace_id: wid.clone(),
                name: "Same".into(),
                identifier: "A".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let err = create(
            &conn,
            CreateProject {
                workspace_id: wid,
                name: "Same".into(),
                identifier: "B".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, crate::error::Error::Duplicate(_)));
    }

    #[test]
    fn delete_soft_then_recreate_same_name() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        let proj = create(
            &conn,
            CreateProject {
                workspace_id: wid.clone(),
                name: "Test".into(),
                identifier: "T".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        delete(&conn, &proj.id).unwrap();
        let proj2 = create(
            &conn,
            CreateProject {
                workspace_id: wid,
                name: "Test".into(),
                identifier: "T".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        assert_eq!(proj2.name, "Test");
    }
}
