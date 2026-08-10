use crate::error::Result;
use rusqlite::{Connection, params};

/// One repo the local GitHub bridge daemon watches — the reverse of
/// `.agentflare/project.json` (folder → project), indexed by repo instead so
/// the daemon (which has no reliable cwd) can enumerate every repo it should
/// poll instead of relying on a single `AGENTFLARE_BRIDGE_REPO` env var.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BridgeRepo {
    pub repo: String,
    pub project_id: String,
    pub folder_path: String,
    pub queue_label: String,
    pub work_agent: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<BridgeRepo> {
    Ok(BridgeRepo {
        repo: row.get(0)?,
        project_id: row.get(1)?,
        folder_path: row.get(2)?,
        queue_label: row.get(3)?,
        work_agent: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

const COLUMNS: &str =
    "repo, project_id, folder_path, queue_label, work_agent, created_at, updated_at";

#[allow(clippy::too_many_arguments)]
pub fn upsert(
    conn: &Connection,
    repo: &str,
    project_id: &str,
    folder_path: &str,
    queue_label: &str,
    work_agent: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO bridge_repos ({COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(repo) DO UPDATE SET
               project_id = excluded.project_id,
               folder_path = excluded.folder_path,
               queue_label = excluded.queue_label,
               work_agent = excluded.work_agent,
               updated_at = excluded.updated_at"
        ),
        params![repo, project_id, folder_path, queue_label, work_agent, now],
    )?;
    Ok(())
}

pub fn list(conn: &Connection) -> Result<Vec<BridgeRepo>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM bridge_repos ORDER BY repo"))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_project(conn: &Connection) -> String {
        let workspace = crate::workspace::create(
            conn,
            crate::workspace::CreateWorkspace {
                name: "ws".into(),
                slug: "ws".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let project = crate::project::create(
            conn,
            crate::project::CreateProject {
                workspace_id: workspace.id,
                name: "proj".into(),
                identifier: "proj".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        project.id
    }

    #[test]
    fn upsert_then_list_round_trips() {
        let conn = crate::db::open_in_memory().unwrap();
        let pid = seed_project(&conn);
        upsert(
            &conn,
            "getappz/agentflare",
            &pid,
            "/home/avihs/projects/agentflare",
            "agentflare",
            None,
            100,
        )
        .unwrap();

        let rows = list(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo, "getappz/agentflare");
        assert_eq!(rows[0].project_id, pid);
        assert_eq!(rows[0].work_agent, None);
        assert_eq!(rows[0].created_at, 100);
        assert_eq!(rows[0].updated_at, 100);
    }

    #[test]
    fn upsert_on_existing_repo_updates_in_place() {
        let conn = crate::db::open_in_memory().unwrap();
        let pid = seed_project(&conn);
        upsert(
            &conn,
            "getappz/agentflare",
            &pid,
            "/old/path",
            "agentflare",
            None,
            100,
        )
        .unwrap();
        upsert(
            &conn,
            "getappz/agentflare",
            &pid,
            "/new/path",
            "agentflare",
            Some("claude-code"),
            200,
        )
        .unwrap();

        let rows = list(&conn).unwrap();
        assert_eq!(rows.len(), 1, "same repo must not create a second row");
        assert_eq!(rows[0].folder_path, "/new/path");
        assert_eq!(rows[0].work_agent.as_deref(), Some("claude-code"));
        assert_eq!(rows[0].created_at, 100, "created_at is not overwritten");
        assert_eq!(rows[0].updated_at, 200);
    }
}
