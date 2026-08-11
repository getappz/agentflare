use crate::error::Result;
use rusqlite::{Connection, params};

/// One project's on-disk repo root — the reverse of `.agentflare/project.json`
/// (folder → project), indexed by project instead so a process with no
/// reliable cwd of its own (the daemon's background discovery loop) can
/// enumerate every project's folder it should operate against. Refreshed by
/// `resolve_project` wherever an agentflare CLI/MCP call runs inside a
/// linked repo. Unlike `bridge_repo`, not limited to GitHub-hosted repos —
/// every linked project gets a row here, regardless of its remote.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectDir {
    pub project_id: String,
    pub folder_path: String,
    pub updated_at: i64,
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<ProjectDir> {
    Ok(ProjectDir {
        project_id: row.get(0)?,
        folder_path: row.get(1)?,
        updated_at: row.get(2)?,
    })
}

pub fn upsert(conn: &Connection, project_id: &str, folder_path: &str, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO project_dirs (project_id, folder_path, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(project_id) DO UPDATE SET
           folder_path = excluded.folder_path,
           updated_at = excluded.updated_at",
        params![project_id, folder_path, now],
    )?;
    Ok(())
}

pub fn list(conn: &Connection) -> Result<Vec<ProjectDir>> {
    let mut stmt = conn.prepare(
        "SELECT project_id, folder_path, updated_at FROM project_dirs ORDER BY project_id",
    )?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_project(conn: &Connection, name: &str) -> String {
        let workspace = crate::workspace::create(
            conn,
            crate::workspace::CreateWorkspace {
                name: name.into(),
                slug: name.into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let project = crate::project::create(
            conn,
            crate::project::CreateProject {
                workspace_id: workspace.id,
                name: name.into(),
                identifier: name.into(),
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
        let pid = seed_project(&conn, "proj");
        upsert(&conn, &pid, "/home/avihs/projects/agentflare", 100).unwrap();

        let rows = list(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project_id, pid);
        assert_eq!(rows[0].folder_path, "/home/avihs/projects/agentflare");
        assert_eq!(rows[0].updated_at, 100);
    }

    #[test]
    fn upsert_on_existing_project_updates_in_place() {
        let conn = crate::db::open_in_memory().unwrap();
        let pid = seed_project(&conn, "proj");
        upsert(&conn, &pid, "/old/path", 100).unwrap();
        upsert(&conn, &pid, "/new/path", 200).unwrap();

        let rows = list(&conn).unwrap();
        assert_eq!(rows.len(), 1, "same project must not create a second row");
        assert_eq!(rows[0].folder_path, "/new/path");
        assert_eq!(rows[0].updated_at, 200);
    }

    #[test]
    fn list_returns_every_registered_project() {
        let conn = crate::db::open_in_memory().unwrap();
        let p1 = seed_project(&conn, "one");
        let p2 = seed_project(&conn, "two");
        upsert(&conn, &p1, "/repo/one", 1).unwrap();
        upsert(&conn, &p2, "/repo/two", 2).unwrap();

        let rows = list(&conn).unwrap();
        assert_eq!(rows.len(), 2);
    }
}
