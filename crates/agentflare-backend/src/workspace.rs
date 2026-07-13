use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub owner_agent: Option<String>,
    pub item_label: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkspace {
    pub name: String,
    pub slug: String,
    pub owner_agent: Option<String>,
    pub item_label: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateWorkspace {
    pub name: Option<String>,
    pub item_label: Option<String>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_to_workspace(row: &rusqlite::Row) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
        slug: row.get(2)?,
        owner_agent: row.get(3)?,
        item_label: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        deleted_at: row.get(7)?,
    })
}

pub fn create(conn: &Connection, input: CreateWorkspace) -> Result<Workspace> {
    let id = uuid::Uuid::now_v7().to_string();
    let ts = now();
    let item_label = input.item_label.unwrap_or_else(|| "Item".to_string());
    conn.execute(
        "INSERT INTO workspaces (id, name, slug, owner_agent, item_label, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            input.name,
            input.slug,
            input.owner_agent,
            item_label,
            ts,
            ts
        ],
    )?;
    get(conn, &id)
}

pub fn get(conn: &Connection, id: &str) -> Result<Workspace> {
    conn.query_row(
        "SELECT id, name, slug, owner_agent, item_label, created_at, updated_at, deleted_at
         FROM workspaces WHERE id = ?1 AND deleted_at IS NULL",
        params![id],
        row_to_workspace,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(id.to_string()),
        other => other.into(),
    })
}

pub fn list(conn: &Connection) -> Result<Vec<Workspace>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, slug, owner_agent, item_label, created_at, updated_at, deleted_at
         FROM workspaces WHERE deleted_at IS NULL ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], row_to_workspace)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn update(conn: &Connection, id: &str, input: UpdateWorkspace) -> Result<Workspace> {
    let ts = now();
    let mut sets = vec!["updated_at = ?2".to_string()];
    let mut param_idx = 3;
    if input.name.is_some() {
        sets.push(format!("name = ?{param_idx}"));
        param_idx += 1;
    }
    if input.item_label.is_some() {
        sets.push(format!("item_label = ?{param_idx}"));
    }
    let sql = format!(
        "UPDATE workspaces SET {} WHERE id = ?1 AND deleted_at IS NULL",
        sets.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(id.to_string()));
    param_values.push(Box::new(ts));
    if let Some(ref name) = input.name {
        param_values.push(Box::new(name.clone()));
    }
    if let Some(ref label) = input.item_label {
        param_values.push(Box::new(label.clone()));
    }
    let changed = stmt.execute(rusqlite::params_from_iter(param_values.iter()))?;
    if changed == 0 {
        return Err(Error::NotFound(id.to_string()));
    }
    get(conn, id)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let ts = now();
    let changed = conn.execute(
        "UPDATE workspaces SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
        params![ts, id],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(id.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn mem() -> Connection {
        db::open_in_memory().unwrap()
    }

    #[test]
    fn create_and_get() {
        let conn = mem();
        let ws = create(
            &conn,
            CreateWorkspace {
                name: "Test".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        assert_eq!(ws.name, "Test");
        assert_eq!(ws.slug, "test");
        assert_eq!(ws.item_label, "Item");
        let got = get(&conn, &ws.id).unwrap();
        assert_eq!(got.id, ws.id);
    }

    #[test]
    fn get_not_found() {
        let conn = db::open_in_memory().unwrap();
        match get(&conn, "nonexistent") {
            Err(Error::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_slug_fails() {
        let conn = db::open_in_memory().unwrap();
        create(
            &conn,
            CreateWorkspace {
                name: "A".into(),
                slug: "same".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let err = create(
            &conn,
            CreateWorkspace {
                name: "B".into(),
                slug: "same".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Duplicate(_)));
    }

    #[test]
    fn delete_soft_and_recreate() {
        let conn = db::open_in_memory().unwrap();
        let ws = create(
            &conn,
            CreateWorkspace {
                name: "Test".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        delete(&conn, &ws.id).unwrap();
        assert!(matches!(get(&conn, &ws.id), Err(Error::NotFound(_))));
        // Can recreate with same slug after soft-delete
        let ws2 = create(
            &conn,
            CreateWorkspace {
                name: "Test2".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        assert_eq!(ws2.slug, "test");
    }

    #[test]
    fn list_returns_all() {
        let conn = db::open_in_memory().unwrap();
        create(
            &conn,
            CreateWorkspace {
                name: "A".into(),
                slug: "a".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        create(
            &conn,
            CreateWorkspace {
                name: "B".into(),
                slug: "b".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn update_mutates_fields() {
        let conn = db::open_in_memory().unwrap();
        let ws = create(
            &conn,
            CreateWorkspace {
                name: "Original".into(),
                slug: "orig".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let updated = update(
            &conn,
            &ws.id,
            UpdateWorkspace {
                name: Some("Updated".into()),
                item_label: Some("Issue".into()),
            },
        )
        .unwrap();
        assert_eq!(updated.name, "Updated");
        assert_eq!(updated.item_label, "Issue");
        assert_eq!(ws.created_at, ws.updated_at);
        assert!(
            updated.created_at == ws.created_at,
            "created_at should not change on update"
        );
    }

    #[test]
    fn delete_then_get_returns_not_found() {
        let conn = db::open_in_memory().unwrap();
        let ws = create(
            &conn,
            CreateWorkspace {
                name: "Test".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        delete(&conn, &ws.id).unwrap();
        assert!(matches!(get(&conn, &ws.id), Err(Error::NotFound(_))));
    }

    #[test]
    fn delete_nonexistent_returns_not_found() {
        let conn = db::open_in_memory().unwrap();
        assert!(matches!(
            delete(&conn, "nonexistent"),
            Err(Error::NotFound(_))
        ));
    }
}
