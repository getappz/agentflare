use crate::error::Result;
use rusqlite::{Connection, params};

/// One "the supervisor asked a human" occurrence — the persisted counterpart
/// of `quota::decide::Decision::ask()`, which itself never writes anything
/// (it's pure). Recorded so attention cost stops being an ephemeral side
/// effect of a lifecycle flip and becomes queryable for performance review.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AskEvent {
    pub id: String,
    pub project_id: String,
    pub goal_item_id: Option<String>,
    pub item_id: String,
    pub agent: Option<String>,
    pub reason: String,
    pub gate_question: Option<String>,
    pub created_at: i64,
}

#[allow(clippy::too_many_arguments)]
pub fn record(
    conn: &Connection,
    project_id: &str,
    goal_item_id: Option<&str>,
    item_id: &str,
    agent: Option<&str>,
    reason: &str,
    gate_question: Option<&str>,
    now: i64,
) -> Result<String> {
    let id = db_kit::ids::new_id();
    conn.execute(
        "INSERT INTO ask_events
             (id, project_id, goal_item_id, item_id, agent, reason, gate_question, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            project_id,
            goal_item_id,
            item_id,
            agent,
            reason,
            gate_question,
            now
        ],
    )?;
    Ok(id)
}

pub fn count_since(
    conn: &Connection,
    project_id: &str,
    agent: Option<&str>,
    since: i64,
) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM ask_events
         WHERE project_id = ?1 AND created_at >= ?2 AND (?3 IS NULL OR agent = ?3)",
        params![project_id, since, agent],
        |r| r.get(0),
    )?)
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
    fn record_then_count_since_scopes_by_project_and_agent() {
        let conn = crate::db::open_in_memory().unwrap();
        let pid = seed_project(&conn);
        record(
            &conn,
            &pid,
            None,
            "item-1",
            Some("claude-code"),
            "gated",
            Some("q?"),
            100,
        )
        .unwrap();
        record(
            &conn,
            &pid,
            None,
            "item-2",
            Some("opencode"),
            "gated",
            None,
            200,
        )
        .unwrap();

        assert_eq!(count_since(&conn, &pid, None, 0).unwrap(), 2);
        assert_eq!(count_since(&conn, &pid, Some("claude-code"), 0).unwrap(), 1);
        assert_eq!(count_since(&conn, &pid, None, 150).unwrap(), 1);
    }
}
