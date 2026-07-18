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
        crate::claims::list(
            &conn,
            None,
            true,
            crate::claims::now(),
            crate::claims::ttl_secs(),
        )
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

/// Comments on an item as a JSON array string; reuses
/// `agentflare_backend::comment::list_by_item`. "[]" on error.
pub fn comments_json(item_id: &str) -> String {
    match pm_db_readonly() {
        Ok(conn) => comments_json_from(&conn, item_id),
        Err(_) => "[]".into(),
    }
}

fn comments_json_from(conn: &Connection, item_id: &str) -> String {
    match agentflare_backend::comment::list_by_item(conn, item_id) {
        Ok(rows) => serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into()),
        Err(_) => "[]".into(),
    }
}

/// Labels as a JSON array string, scoped to a project (if `project_id` is
/// given) or else a workspace (if only `workspace_id` is given); reuses
/// `agentflare_backend::label::{list_by_project, list_by_workspace}`. "[]" if
/// neither scope is given, or on error.
pub fn labels_json(workspace_id: Option<&str>, project_id: Option<&str>) -> String {
    match pm_db_readonly() {
        Ok(conn) => labels_json_from(&conn, workspace_id, project_id),
        Err(_) => "[]".into(),
    }
}

fn labels_json_from(
    conn: &Connection,
    workspace_id: Option<&str>,
    project_id: Option<&str>,
) -> String {
    let result = match (project_id, workspace_id) {
        (Some(pid), _) => agentflare_backend::label::list_by_project(conn, pid),
        (None, Some(wid)) => agentflare_backend::label::list_by_workspace(conn, wid),
        (None, None) => return "[]".into(),
    };
    match result {
        Ok(rows) => serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into()),
        Err(_) => "[]".into(),
    }
}

/// Webhook delivery log entries for a workspace as a JSON array string — the
/// closest thing to a PM "events" audit trail on this backend; reuses
/// `agentflare_backend::webhook::list_logs_by_workspace`. "[]" on error.
pub fn webhooks_json(workspace_id: &str) -> String {
    match pm_db_readonly() {
        Ok(conn) => webhooks_json_from(&conn, workspace_id),
        Err(_) => "[]".into(),
    }
}

fn webhooks_json_from(conn: &Connection, workspace_id: &str) -> String {
    match agentflare_backend::webhook::list_logs_by_workspace(conn, workspace_id) {
        Ok(rows) => serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into()),
        Err(_) => "[]".into(),
    }
}

/// Cost summary for the last `days` days (inclusive of today), grouped by
/// `by` — "project" groups by project, anything else by model — as a JSON
/// object `{ groups: [...], total_cost_usd, any_unpriced }`. Reuses
/// `crate::cost::summarize`, so it opens+syncs the same analytics cache the
/// `agentflare cost` CLI does and reports identical numbers.
pub fn cost_json(days: u32, by: &str) -> String {
    let today = chrono::Local::now().date_naive();
    let window = days.max(1);
    let start = today - chrono::Duration::days(window as i64 - 1);
    let group_by = match by {
        "project" => crate::cost::GroupBy::Project,
        _ => crate::cost::GroupBy::Model,
    };
    cost_totals_to_json(&crate::cost::summarize((start, today), group_by))
}

/// Combine an already-computed `claims` (JSON array string) and `cost_today`
/// (JSON object string) into the `/events` snapshot `{ claims, cost_today }`.
/// Re-parses each so they nest as JSON values rather than embedded strings;
/// malformed or empty input degrades to `[]` / `{}`, so this never fails.
pub fn snapshot_json(claims_json_str: &str, cost_today_json_str: &str) -> String {
    let claims: serde_json::Value =
        serde_json::from_str(claims_json_str).unwrap_or_else(|_| serde_json::json!([]));
    let cost_today: serde_json::Value =
        serde_json::from_str(cost_today_json_str).unwrap_or_else(|_| serde_json::json!({}));
    serde_json::json!({ "claims": claims, "cost_today": cost_today }).to_string()
}

/// Shape per-group cost totals into the `/api/cost` JSON. Split out from
/// `cost_json` so it can be unit-tested without opening the analytics cache
/// or touching the filesystem. Groups are sorted by key for stable output.
fn cost_totals_to_json(
    totals: &std::collections::HashMap<String, crate::cost::GroupTotals>,
) -> String {
    let mut rows: Vec<_> = totals.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));

    let mut total_cost_usd = 0.0_f64;
    let mut any_unpriced = false;
    let groups: Vec<serde_json::Value> = rows
        .iter()
        .map(|(key, g)| {
            total_cost_usd += g.cost_usd;
            any_unpriced |= g.has_unpriced_usage;
            serde_json::json!({
                "key": key,
                "input_tokens": g.tokens.input_tokens,
                "output_tokens": g.tokens.output_tokens,
                "cache_read_tokens": g.tokens.cache_read_tokens,
                "cache_creation_tokens": g.tokens.cache_creation_tokens,
                "cost_usd": g.cost_usd,
                "has_unpriced_usage": g.has_unpriced_usage,
            })
        })
        .collect();

    serde_json::json!({
        "groups": groups,
        "total_cost_usd": total_cost_usd,
        "any_unpriced": any_unpriced,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn cost_totals_to_json_shapes_groups_total_and_unpriced() {
        use crate::cost::GroupTotals;
        use crate::pricing::TokenUsage;
        let mut totals = std::collections::HashMap::new();
        totals.insert(
            "claude-opus-4-8".to_string(),
            GroupTotals {
                tokens: TokenUsage {
                    input_tokens: 100,
                    output_tokens: 50,
                    ..Default::default()
                },
                cost_usd: 1.25,
                has_unpriced_usage: false,
            },
        );
        totals.insert(
            "some-unpriced-model".to_string(),
            GroupTotals {
                tokens: TokenUsage {
                    input_tokens: 10,
                    ..Default::default()
                },
                cost_usd: 0.0,
                has_unpriced_usage: true,
            },
        );

        let json = cost_totals_to_json(&totals);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "one row per group");
        // Sorted by key: opus first, unpriced second.
        assert_eq!(groups[0]["key"], "claude-opus-4-8");
        assert_eq!(groups[0]["input_tokens"], 100);
        assert_eq!(groups[0]["output_tokens"], 50);
        assert_eq!(groups[0]["cost_usd"], 1.25);
        assert_eq!(groups[1]["key"], "some-unpriced-model");

        assert_eq!(v["total_cost_usd"], 1.25, "total is the sum of group costs");
        assert_eq!(
            v["any_unpriced"], true,
            "flagged when any group is unpriced"
        );
    }

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
        assert!(
            format!("{err}").contains("read"),
            "must reject writes: {err}"
        );
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
            assert!(
                format!("{err}").contains("read"),
                "must reject writes: {err}"
            );
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
        assert!(
            json.contains("\"slug\":\"acme\""),
            "expected acme workspace in {json}"
        );
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
        assert!(
            json.contains("\"identifier\":\"ROCK\""),
            "expected ROCK project in {json}"
        );
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
        assert!(
            json.contains("\"group_name\":\"backlog\""),
            "expected default states in {json}"
        );
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
        assert!(
            json.contains("\"name\":\"Fix bug\""),
            "expected item in {json}"
        );
        let empty = items_json_from(&conn, "nonexistent-project");
        assert_eq!(empty, "[]");
    }

    #[test]
    fn comments_json_from_scopes_to_item() {
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
        let item = agentflare_backend::item::create(
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
        agentflare_backend::comment::create(&conn, &item.id, "agent-1", "Looks good").unwrap();
        let json = comments_json_from(&conn, &item.id);
        assert!(
            json.contains("\"body\":\"Looks good\""),
            "expected comment in {json}"
        );
        let empty = comments_json_from(&conn, "nonexistent-item");
        assert_eq!(empty, "[]");
    }

    #[test]
    fn labels_json_from_scopes_by_project_then_workspace() {
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
        agentflare_backend::label::create(
            &conn,
            agentflare_backend::label::CreateLabel {
                project_id: Some(proj.id.clone()),
                workspace_id: ws.id.clone(),
                name: "bug".into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let by_project = labels_json_from(&conn, None, Some(&proj.id));
        assert!(
            by_project.contains("\"name\":\"bug\""),
            "expected label in {by_project}"
        );
        let by_workspace = labels_json_from(&conn, Some(&ws.id), None);
        assert!(
            by_workspace.contains("\"name\":\"bug\""),
            "expected label in {by_workspace}"
        );
        let neither = labels_json_from(&conn, None, None);
        assert_eq!(neither, "[]");
    }

    #[test]
    fn webhooks_json_from_scopes_to_workspace() {
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
        let wh = agentflare_backend::webhook::create(
            &conn,
            agentflare_backend::webhook::CreateWebhook {
                workspace_id: ws.id.clone(),
                url: "https://example.invalid/hook".into(),
                secret_key: "s3cret".into(),
                on_item: Some(true),
                on_state: None,
                on_project: None,
            },
        )
        .unwrap();
        agentflare_backend::webhook::deliver(
            &conn,
            &wh,
            "item",
            "create",
            serde_json::json!({"id": "1"}),
        )
        .unwrap();
        let json = webhooks_json_from(&conn, &ws.id);
        assert!(
            json.contains("\"event_type\":\"item\""),
            "expected event log in {json}"
        );
        let empty = webhooks_json_from(&conn, "nonexistent-workspace");
        assert_eq!(empty, "[]");
    }
}
