use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    pub project_id: Option<String>,
    pub workspace_id: String,
    pub name: String,
    pub color: String,
    pub parent_id: Option<String>,
    pub sort_order: f64,
    pub external_source: Option<String>,
    pub external_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLabel {
    pub project_id: Option<String>,
    pub workspace_id: String,
    pub name: String,
    pub color: Option<String>,
    pub parent_id: Option<String>,
    pub sort_order: Option<f64>,
    pub external_source: Option<String>,
    pub external_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateLabel {
    pub name: Option<String>,
    pub color: Option<String>,
    pub sort_order: Option<f64>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_to_label(row: &rusqlite::Row) -> rusqlite::Result<Label> {
    Ok(Label {
        id: row.get(0)?,
        project_id: row.get(1)?,
        workspace_id: row.get(2)?,
        name: row.get(3)?,
        color: row.get(4)?,
        parent_id: row.get(5)?,
        sort_order: row.get(6)?,
        external_source: row.get(7)?,
        external_id: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        deleted_at: row.get(11)?,
    })
}

/// Next sort_order for a new label: `max(sort_order) + 10000` within the same
/// scope (project for project-scoped labels, workspace for workspace-level ones),
/// or 65535 when the scope has no labels yet. Mirrors Plane's append-on-create.
fn next_sort_order(conn: &Connection, project_id: Option<&str>, workspace_id: &str) -> Result<f64> {
    let max: Option<f64> = match project_id {
        Some(pid) => conn.query_row(
            "SELECT MAX(sort_order) FROM labels WHERE project_id = ?1 AND deleted_at IS NULL",
            rusqlite::params![pid],
            |row| row.get(0),
        )?,
        None => conn.query_row(
            "SELECT MAX(sort_order) FROM labels WHERE workspace_id = ?1 AND project_id IS NULL AND deleted_at IS NULL",
            rusqlite::params![workspace_id],
            |row| row.get(0),
        )?,
    };
    Ok(max.map_or(65535.0, |m| m + 10000.0))
}

pub fn create(conn: &Connection, input: CreateLabel) -> Result<Label> {
    let id = db_kit::ids::new_id();
    let ts = now();
    let color = input.color.unwrap_or_else(|| "#60646C".to_string());
    // Compute the auto-append sort_order and insert inside one transaction so two
    // concurrent auto-append creates can't read the same MAX and collide.
    let tx = conn.unchecked_transaction()?;
    let sort_order = match input.sort_order {
        Some(v) => v,
        None => next_sort_order(&tx, input.project_id.as_deref(), &input.workspace_id)?,
    };
    tx.execute(
        "INSERT INTO labels (id, project_id, workspace_id, name, color, parent_id, sort_order, external_source, external_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            id,
            input.project_id,
            input.workspace_id,
            input.name,
            color,
            input.parent_id,
            sort_order,
            input.external_source,
            input.external_id,
            ts,
            ts,
        ],
    )?;
    tx.commit()?;
    get(conn, &id)
}

pub fn get(conn: &Connection, id: &str) -> Result<Label> {
    conn.query_row(
        "SELECT id, project_id, workspace_id, name, color, parent_id, sort_order, external_source, external_id, created_at, updated_at, deleted_at
         FROM labels WHERE id = ?1 AND deleted_at IS NULL",
        rusqlite::params![id],
        row_to_label,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => crate::error::Error::NotFound(id.to_string()),
        other => other.into(),
    })
}

pub fn list_by_workspace(conn: &Connection, workspace_id: &str) -> Result<Vec<Label>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, workspace_id, name, color, parent_id, sort_order, external_source, external_id, created_at, updated_at, deleted_at
         FROM labels WHERE workspace_id = ?1 AND deleted_at IS NULL ORDER BY sort_order",
    )?;
    let rows = stmt.query_map(rusqlite::params![workspace_id], row_to_label)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn list_by_project(conn: &Connection, project_id: &str) -> Result<Vec<Label>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, workspace_id, name, color, parent_id, sort_order, external_source, external_id, created_at, updated_at, deleted_at
         FROM labels WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY sort_order",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id], row_to_label)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn update(conn: &Connection, id: &str, input: UpdateLabel) -> Result<Label> {
    let ts = now();
    let mut sets = vec!["updated_at = ?2".to_string()];
    let mut param_idx = 3;
    if input.name.is_some() {
        sets.push(format!("name = ?{param_idx}"));
        param_idx += 1;
    }
    if input.color.is_some() {
        sets.push(format!("color = ?{param_idx}"));
        param_idx += 1;
    }
    if input.sort_order.is_some() {
        sets.push(format!("sort_order = ?{param_idx}"));
    }
    let sql = format!(
        "UPDATE labels SET {} WHERE id = ?1 AND deleted_at IS NULL",
        sets.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(id.to_string()));
    param_values.push(Box::new(ts));
    if let Some(ref name) = input.name {
        param_values.push(Box::new(name.clone()));
    }
    if let Some(ref color) = input.color {
        param_values.push(Box::new(color.clone()));
    }
    if let Some(so) = input.sort_order {
        param_values.push(Box::new(so));
    }
    let changed = stmt.execute(rusqlite::params_from_iter(param_values.iter()))?;
    if changed == 0 {
        return Err(crate::error::Error::NotFound(id.to_string()));
    }
    get(conn, id)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let ts = now();
    let changed = conn.execute(
        "UPDATE labels SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
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
        let label = create(
            &conn,
            CreateLabel {
                project_id: None,
                workspace_id: wid,
                name: "bug".into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        assert_eq!(label.name, "bug");
        let got = get(&conn, &label.id).unwrap();
        assert_eq!(got.id, label.id);
    }

    #[test]
    fn create_auto_appends_sort_order() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        let mk = |name: &str, sort_order: Option<f64>| {
            create(
                &conn,
                CreateLabel {
                    project_id: None,
                    workspace_id: wid.clone(),
                    name: name.into(),
                    color: None,
                    parent_id: None,
                    sort_order,
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap()
        };
        // First label in the scope seeds at 65535, each next appends +10000.
        assert_eq!(mk("a", None).sort_order, 65535.0);
        assert_eq!(mk("b", None).sort_order, 75535.0);
        // An explicit sort_order is respected verbatim and doesn't shift the max.
        assert_eq!(mk("c", Some(5.0)).sort_order, 5.0);
        assert_eq!(mk("d", None).sort_order, 85535.0);
    }

    #[test]
    fn duplicate_name_in_workspace_fails() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        create(
            &conn,
            CreateLabel {
                project_id: None,
                workspace_id: wid.clone(),
                name: "bug".into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let err = create(
            &conn,
            CreateLabel {
                project_id: None,
                workspace_id: wid,
                name: "bug".into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, crate::error::Error::Duplicate(_)));
    }

    #[test]
    fn list_labels_by_workspace() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        create(
            &conn,
            CreateLabel {
                project_id: None,
                workspace_id: wid.clone(),
                name: "bug".into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        create(
            &conn,
            CreateLabel {
                project_id: None,
                workspace_id: wid.clone(),
                name: "feature".into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        assert_eq!(list_by_workspace(&conn, &wid).unwrap().len(), 2);
    }
}
