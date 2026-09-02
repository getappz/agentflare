use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::Result;

mod claim;
mod crud;
mod relations;
mod search;
#[cfg(test)]
mod tests;

pub use claim::*;
pub use crud::*;
pub use relations::*;
pub use search::*;

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
    pub start_date: Option<i64>,
    pub due_date: Option<i64>,
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
    pub start_date: Option<i64>,
    pub due_date: Option<i64>,
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
    /// Re-parenting is three-valued, unlike every other field here:
    /// `None` leaves the parent alone, `Some(Some(id))` re-parents, and
    /// `Some(None)` detaches the item from its parent — otherwise a parent
    /// set by mistake could never be removed again.
    pub parent_id: Option<Option<String>>,
    pub start_date: Option<i64>,
    pub due_date: Option<i64>,
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
        start_date: row.get(19)?,
        due_date: row.get(20)?,
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
