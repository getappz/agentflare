use crate::error::{Error, Result};
use rusqlite::{Connection, OptionalExtension, params};

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

/// Refuses to associate `folder_path` with `project_id` when it's already
/// registered to a DIFFERENT project (item #303: a stray project-override
/// run from the wrong cwd once silently overwrote another project's row,
/// producing an N:1 folder→project mapping that then pointed the daemon's
/// cwd-less sweep at the wrong project entirely). First registration wins;
/// re-registering the SAME project's row still updates in place.
pub fn upsert(conn: &Connection, project_id: &str, folder_path: &str, now: i64) -> Result<()> {
    let existing_owner: Option<String> = conn
        .query_row(
            "SELECT project_id FROM project_dirs WHERE folder_path = ?1",
            params![folder_path],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(owner) = existing_owner
        && owner != project_id
    {
        return Err(Error::Duplicate(format!(
            "folder_path {folder_path} is already registered to project {owner}, \
             refusing to also register it to {project_id}"
        )));
    }
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

/// Folders of every *active* project: rows whose project is neither
/// soft-deleted nor archived. This is what the daemon's discovery tick and
/// review sweep enumerate, so a soft-deleted project (its row used to
/// survive, only a hard delete cascades) or an archived one (item #768:
/// `projects.archived_at` was never read anywhere, so archiving had no
/// effect on dispatch) no longer gets swept, dispatched, or logged every
/// tick. Use `list_all` to map a folder back to its project regardless of
/// that project's state.
pub fn list(conn: &Connection) -> Result<Vec<ProjectDir>> {
    let mut stmt = conn.prepare(
        "SELECT d.project_id, d.folder_path, d.updated_at
         FROM project_dirs d
         JOIN projects p ON p.id = d.project_id
         WHERE p.deleted_at IS NULL AND p.archived_at IS NULL
         ORDER BY d.project_id",
    )?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every registered folder, including those of archived or soft-deleted
/// projects — for callers that start from a folder or an item and need its
/// project (`flare doctor` inside an archived repo, item controls), not for
/// anything that decides which projects to act on.
pub fn list_all(conn: &Connection) -> Result<Vec<ProjectDir>> {
    let mut stmt = conn.prepare(
        "SELECT project_id, folder_path, updated_at FROM project_dirs ORDER BY project_id",
    )?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Drops `project_id`'s folder row. Called by `project::delete` so a
/// soft-deleted project's folder stops being enumerated (the schema's
/// `ON DELETE CASCADE` only fires on a hard delete).
pub fn remove(conn: &Connection, project_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM project_dirs WHERE project_id = ?1",
        params![project_id],
    )?;
    Ok(())
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
    fn upsert_refuses_a_folder_path_already_claimed_by_another_project() {
        let conn = crate::db::open_in_memory().unwrap();
        let p1 = seed_project(&conn, "one");
        let p2 = seed_project(&conn, "two");
        upsert(&conn, &p1, "/shared/path", 100).unwrap();

        let err = upsert(&conn, &p2, "/shared/path", 200).unwrap_err();
        assert!(matches!(err, crate::error::Error::Duplicate(_)));

        let rows = list(&conn).unwrap();
        assert_eq!(rows.len(), 1, "second project must not steal the row");
        assert_eq!(rows[0].project_id, p1, "original owner keeps the folder");
        assert_eq!(
            rows[0].updated_at, 100,
            "refused write must not touch the row"
        );
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

    #[test]
    fn list_skips_soft_deleted_and_archived_projects_but_list_all_keeps_them() {
        let conn = crate::db::open_in_memory().unwrap();
        let live = seed_project(&conn, "live");
        let archived = seed_project(&conn, "archived");
        let deleted = seed_project(&conn, "deleted");
        upsert(&conn, &live, "/repo/live", 1).unwrap();
        upsert(&conn, &archived, "/repo/archived", 2).unwrap();
        upsert(&conn, &deleted, "/repo/deleted", 3).unwrap();

        crate::project::archive(&conn, &archived).unwrap();
        // Bypass `project::delete` (which also removes the dir row) to prove
        // the filter alone hides a pre-existing stale row — the exact shape
        // of the 29 Temp\.tmp* rows from item #768.
        conn.execute(
            "UPDATE projects SET deleted_at = 10 WHERE id = ?1",
            params![deleted],
        )
        .unwrap();

        let active: Vec<String> = list(&conn)
            .unwrap()
            .into_iter()
            .map(|d| d.project_id)
            .collect();
        assert_eq!(active, vec![live.clone()]);

        let all = list_all(&conn).unwrap();
        assert_eq!(all.len(), 3, "list_all must keep every registered folder");

        crate::project::unarchive(&conn, &archived).unwrap();
        let mut active: Vec<String> = list(&conn)
            .unwrap()
            .into_iter()
            .map(|d| d.project_id)
            .collect();
        active.sort();
        let mut expected = vec![live, archived];
        expected.sort();
        assert_eq!(active, expected, "unarchive needs no re-registration");
    }

    #[test]
    fn project_soft_delete_removes_its_folder_row() {
        let conn = crate::db::open_in_memory().unwrap();
        let pid = seed_project(&conn, "proj");
        upsert(&conn, &pid, "/repo/proj", 1).unwrap();

        crate::project::delete(&conn, &pid).unwrap();

        assert!(list_all(&conn).unwrap().is_empty());
    }
}
