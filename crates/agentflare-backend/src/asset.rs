use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub workspace_id: Option<String>,
    pub entity_type: String,
    pub entity_id: String,
    pub filename: String,
    pub size: i64,
    pub storage_path: String,
    pub mime_type: Option<String>,
    pub metadata: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAsset {
    pub workspace_id: Option<String>,
    pub entity_type: String,
    pub entity_id: String,
    pub filename: String,
    pub size: i64,
    pub mime_type: Option<String>,
    pub metadata: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateAsset {
    pub filename: Option<String>,
    pub size: Option<i64>,
    pub mime_type: Option<String>,
    pub metadata: Option<String>,
}

pub fn storage_path(workspace_id: &str, filename: &str) -> String {
    let id = uuid::Uuid::now_v7().to_string();
    format!("{}/assets/{}-{}", workspace_id, id, filename)
}

pub fn write_file(
    base_path: &std::path::Path,
    storage_path: &str,
    data: &[u8],
) -> std::io::Result<()> {
    let full_path = base_path.join(storage_path);
    if let Some(parent) = full_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&full_path, data)
}

pub fn read_file(base_path: &std::path::Path, storage_path: &str) -> std::io::Result<Vec<u8>> {
    std::fs::read(base_path.join(storage_path))
}

pub fn delete_file(base_path: &std::path::Path, storage_path: &str) -> std::io::Result<()> {
    let path = base_path.join(storage_path);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_to_asset(row: &rusqlite::Row) -> rusqlite::Result<Asset> {
    Ok(Asset {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        entity_type: row.get(2)?,
        entity_id: row.get(3)?,
        filename: row.get(4)?,
        size: row.get(5)?,
        storage_path: row.get(6)?,
        mime_type: row.get(7)?,
        metadata: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        deleted_at: row.get(11)?,
    })
}

pub fn create(conn: &Connection, input: CreateAsset) -> Result<Asset> {
    let id = uuid::Uuid::now_v7().to_string();
    let ts = now();
    let sp = match &input.workspace_id {
        Some(wid) => storage_path(wid, &input.filename),
        None => {
            let id = uuid::Uuid::now_v7().to_string();
            format!("assets/{}-{}", id, input.filename)
        }
    };
    let metadata = input.metadata.unwrap_or_else(|| "{}".to_string());
    conn.execute(
        "INSERT INTO assets (id, workspace_id, entity_type, entity_id, filename, size, storage_path, mime_type, metadata, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            id,
            input.workspace_id,
            input.entity_type,
            input.entity_id,
            input.filename,
            input.size,
            sp,
            input.mime_type,
            metadata,
            ts,
            ts,
        ],
    )?;
    get(conn, &id)
}

pub fn get(conn: &Connection, id: &str) -> Result<Asset> {
    conn.query_row(
        "SELECT id, workspace_id, entity_type, entity_id, filename, size, storage_path, mime_type, metadata, created_at, updated_at, deleted_at
         FROM assets WHERE id = ?1 AND deleted_at IS NULL",
        rusqlite::params![id],
        row_to_asset,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => crate::error::Error::NotFound(id.to_string()),
        other => other.into(),
    })
}

pub fn list_by_entity(conn: &Connection, entity_type: &str, entity_id: &str) -> Result<Vec<Asset>> {
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, entity_type, entity_id, filename, size, storage_path, mime_type, metadata, created_at, updated_at, deleted_at
         FROM assets WHERE entity_type = ?1 AND entity_id = ?2 AND deleted_at IS NULL ORDER BY created_at",
    )?;
    let rows = stmt.query_map(rusqlite::params![entity_type, entity_id], row_to_asset)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn list_by_workspace(conn: &Connection, workspace_id: &str) -> Result<Vec<Asset>> {
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, entity_type, entity_id, filename, size, storage_path, mime_type, metadata, created_at, updated_at, deleted_at
         FROM assets WHERE workspace_id = ?1 AND deleted_at IS NULL ORDER BY created_at",
    )?;
    let rows = stmt.query_map(rusqlite::params![workspace_id], row_to_asset)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn update(conn: &Connection, id: &str, input: UpdateAsset) -> Result<Asset> {
    let ts = now();
    let mut sets = vec!["updated_at = ?2".to_string()];
    let mut param_idx = 3;
    if input.filename.is_some() {
        sets.push(format!("filename = ?{param_idx}"));
        param_idx += 1;
    }
    if input.size.is_some() {
        sets.push(format!("size = ?{param_idx}"));
        param_idx += 1;
    }
    if input.mime_type.is_some() {
        sets.push(format!("mime_type = ?{param_idx}"));
        param_idx += 1;
    }
    if input.metadata.is_some() {
        sets.push(format!("metadata = ?{param_idx}"));
    }
    let sql = format!(
        "UPDATE assets SET {} WHERE id = ?1 AND deleted_at IS NULL",
        sets.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(id.to_string()));
    param_values.push(Box::new(ts));
    if let Some(ref filename) = input.filename {
        param_values.push(Box::new(filename.clone()));
    }
    if let Some(size) = input.size {
        param_values.push(Box::new(size));
    }
    if let Some(ref mime) = input.mime_type {
        param_values.push(Box::new(mime.clone()));
    }
    if let Some(ref meta) = input.metadata {
        param_values.push(Box::new(meta.clone()));
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
        "UPDATE assets SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
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

    #[test]
    fn storage_path_format() {
        let path = storage_path("ws-42", "photo.jpg");
        assert!(path.starts_with("ws-42/assets/"));
        assert!(path.ends_with("-photo.jpg"));
    }

    #[test]
    fn create_and_get() {
        let conn = db::open_in_memory().unwrap();
        let asset = create(
            &conn,
            CreateAsset {
                workspace_id: Some("ws-1".into()),
                entity_type: "item_attachment".into(),
                entity_id: "item-1".into(),
                filename: "report.pdf".into(),
                size: 1024,
                mime_type: Some("application/pdf".into()),
                metadata: None,
            },
        )
        .unwrap();
        assert_eq!(asset.filename, "report.pdf");
        assert_eq!(asset.size, 1024);
        let got = get(&conn, &asset.id).unwrap();
        assert_eq!(got.id, asset.id);
    }

    #[test]
    fn write_and_read_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_write");
        std::fs::create_dir_all(&path).unwrap();
        let sp = storage_path("ws-1", "test.txt");
        write_file(&path, &sp, b"hello world").unwrap();
        let data = read_file(&path, &sp).unwrap();
        assert_eq!(data, b"hello world");
        delete_file(&path, &sp).unwrap();
        assert!(!path.join(&sp).exists());
    }

    #[test]
    fn list_by_entity_test() {
        let conn = db::open_in_memory().unwrap();
        super::create(
            &conn,
            CreateAsset {
                workspace_id: Some("ws-1".into()),
                entity_type: "item_attachment".into(),
                entity_id: "item-1".into(),
                filename: "a.pdf".into(),
                size: 100,
                mime_type: None,
                metadata: None,
            },
        )
        .unwrap();
        super::create(
            &conn,
            CreateAsset {
                workspace_id: Some("ws-1".into()),
                entity_type: "item_attachment".into(),
                entity_id: "item-1".into(),
                filename: "b.pdf".into(),
                size: 200,
                mime_type: None,
                metadata: None,
            },
        )
        .unwrap();
        super::create(
            &conn,
            CreateAsset {
                workspace_id: Some("ws-1".into()),
                entity_type: "item_attachment".into(),
                entity_id: "item-2".into(),
                filename: "c.pdf".into(),
                size: 300,
                mime_type: None,
                metadata: None,
            },
        )
        .unwrap();
        let assets = super::list_by_entity(&conn, "item_attachment", "item-1").unwrap();
        assert_eq!(assets.len(), 2);
        assert_eq!(
            super::list_by_entity(&conn, "item_attachment", "item-2")
                .unwrap()
                .len(),
            1
        );
    }
}
