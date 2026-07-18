use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// The ONLY way the dashboard opens a database — read-only, so a write can't slip in.
pub fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

/// Open the PM backend database (`~/.agentflare/backend.db`) read-only.
pub fn pm_db_readonly() -> rusqlite::Result<Connection> {
    let path = crate::paths::home().join(".agentflare").join("backend.db");
    open_readonly(&path)
}

/// Live claims as a JSON array string; reuses `crate::claims::list`. "[]" on error.
pub fn claims_json() -> String {
    let path = crate::db::agentflare_db_path();
    let result = open_readonly(&path).and_then(|conn| {
        crate::claims::list(&conn, None, true, crate::claims::now(), crate::claims::ttl_secs())
    });
    match result {
        Ok(claims) => serde_json::to_string(&claims).unwrap_or_else(|_| "[]".into()),
        Err(_) => "[]".into(),
    }
}

/// All PM workspaces as a JSON array string; reuses
/// `agentflare_backend::workspace::list`. "[]" on error.
pub fn workspaces_json() -> String {
    match pm_db_readonly() {
        Ok(conn) => workspaces_json_from(&conn),
        Err(_) => "[]".into(),
    }
}

fn workspaces_json_from(conn: &Connection) -> String {
    match agentflare_backend::workspace::list(conn) {
        Ok(rows) => serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into()),
        Err(_) => "[]".into(),
    }
}

/// Projects in a workspace as a JSON array string; reuses
/// `agentflare_backend::project::list_by_workspace`. "[]" on error.
pub fn projects_json(workspace_id: &str) -> String {
    match pm_db_readonly() {
        Ok(conn) => projects_json_from(&conn, workspace_id),
        Err(_) => "[]".into(),
    }
}

fn projects_json_from(conn: &Connection, workspace_id: &str) -> String {
    match agentflare_backend::project::list_by_workspace(conn, workspace_id) {
        Ok(rows) => serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into()),
        Err(_) => "[]".into(),
    }
}

/// Items in a project as a JSON array string; reuses
/// `agentflare_backend::item::list_by_project`. "[]" on error.
pub fn items_json(project_id: &str) -> String {
    match pm_db_readonly() {
        Ok(conn) => items_json_from(&conn, project_id),
        Err(_) => "[]".into(),
    }
}

fn items_json_from(conn: &Connection, project_id: &str) -> String {
    match agentflare_backend::item::list_by_project(conn, project_id) {
        Ok(rows) => serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into()),
        Err(_) => "[]".into(),
    }
}

/// States in a project as a JSON array string; reuses
/// `agentflare_backend::state::list_by_project`. "[]" on error.
pub fn states_json(project_id: &str) -> String {
    match pm_db_readonly() {
        Ok(conn) => states_json_from(&conn, project_id),
        Err(_) => "[]".into(),
    }
}

fn states_json_from(conn: &Connection, project_id: &str) -> String {
    match agentflare_backend::state::list_by_project(conn, project_id) {
        Ok(rows) => serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into()),
        Err(_) => "[]".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn open_readonly_rejects_writes() {
        let dir = std::env::temp_dir().join("agentflare-test-dash-ro");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        {
            let w = Connection::open(&path).unwrap();
            w.execute_batch("CREATE TABLE t (x INTEGER);").unwrap();
        }
        let ro = open_readonly(&path).unwrap();
        let err = ro.execute("INSERT INTO t (x) VALUES (1)", []).unwrap_err();
        assert!(format!("{err}").contains("read"), "must reject writes: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pm_db_readonly_rejects_writes() {
        crate::paths::test_support::with_temp_home(|| {
            let agentflare_dir = crate::paths::home().join(".agentflare");
            std::fs::create_dir_all(&agentflare_dir).unwrap();
            let path = agentflare_dir.join("backend.db");
            {
                let w = Connection::open(&path).unwrap();
                w.execute_batch("CREATE TABLE t (x INTEGER);").unwrap();
            }
            let ro = pm_db_readonly().unwrap();
            let err = ro.execute("INSERT INTO t (x) VALUES (1)", []).unwrap_err();
            assert!(format!("{err}").contains("read"), "must reject writes: {err}");
        });
    }

    #[test]
    fn workspaces_json_from_serializes_backend_rows() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        agentflare_backend::workspace::create(
            &conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Acme".into(),
                slug: "acme".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let json = workspaces_json_from(&conn);
        assert!(json.contains("\"slug\":\"acme\""), "expected acme workspace in {json}");
    }

    #[test]
    fn projects_json_from_scopes_to_workspace() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let ws = agentflare_backend::workspace::create(
            &conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Acme".into(),
                slug: "acme".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        agentflare_backend::project::create(
            &conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id.clone(),
                name: "Rocket".into(),
                identifier: "ROCK".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let json = projects_json_from(&conn, &ws.id);
        assert!(json.contains("\"identifier\":\"ROCK\""), "expected ROCK project in {json}");
        let empty = projects_json_from(&conn, "nonexistent-workspace");
        assert_eq!(empty, "[]");
    }

    #[test]
    fn states_json_from_scopes_to_project() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let ws = agentflare_backend::workspace::create(
            &conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Acme".into(),
                slug: "acme".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = agentflare_backend::project::create(
            &conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id.clone(),
                name: "Rocket".into(),
                identifier: "ROCK".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let json = states_json_from(&conn, &proj.id);
        assert!(json.contains("\"group_name\":\"backlog\""), "expected default states in {json}");
        let empty = states_json_from(&conn, "nonexistent-project");
        assert_eq!(empty, "[]");
    }

    #[test]
    fn items_json_from_scopes_to_project() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let ws = agentflare_backend::workspace::create(
            &conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Acme".into(),
                slug: "acme".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = agentflare_backend::project::create(
            &conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id.clone(),
                name: "Rocket".into(),
                identifier: "ROCK".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let states = agentflare_backend::state::list_by_project(&conn, &proj.id).unwrap();
        let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        agentflare_backend::item::create(
            &conn,
            agentflare_backend::item::CreateItem {
                project_id: proj.id.clone(),
                state_id,
                name: "Fix bug".into(),
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
        let json = items_json_from(&conn, &proj.id);
        assert!(json.contains("\"name\":\"Fix bug\""), "expected item in {json}");
        let empty = items_json_from(&conn, "nonexistent-project");
        assert_eq!(empty, "[]");
    }
}
