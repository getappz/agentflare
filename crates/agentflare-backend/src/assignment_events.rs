//! Append-only log of item assignee transitions — the persisted "handoff
//! history" the health scorecard's bottleneck signal reads. Rows are written
//! by [`crate::item::update`] whenever `assignee_agent` actually changes
//! (which also covers `claim`, since claiming assigns through `update`).
//! History starts at the migration that shipped the table; transitions
//! before it are unrecorded.

use rusqlite::Connection;

use crate::item::agent_part;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// One item's handoff activity within a window: how many times it moved
/// between *different* agents (instance suffixes stripped — `claude:1` →
/// `claude:2` is not a handoff), and the distinct owner chain in order of
/// first appearance.
#[derive(Debug)]
pub struct HandoffStat {
    pub item_id: String,
    pub handoffs: usize,
    pub owners: Vec<String>,
}

/// Records one assignee transition. Called from `item::update` inside the
/// caller's transaction so the event commits (or rolls back) with the
/// assignment itself.
pub(crate) fn record(
    conn: &Connection,
    item_id: &str,
    from_owner: Option<&str>,
    to_owner: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO item_assignment_events (id, item_id, from_owner, to_owner, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![db_kit::ids::new_id(), item_id, from_owner, to_owner, now()],
    )?;
    Ok(())
}

/// Handoff stats per item for a project, over events at or after `since`.
/// Only items with at least one agent-to-agent handoff are returned; the
/// caller picks its own "repeatedly" threshold.
pub fn handoff_stats_since(
    conn: &Connection,
    project_id: &str,
    since: i64,
) -> crate::error::Result<Vec<HandoffStat>> {
    let mut stmt = conn.prepare(
        "SELECT e.item_id, e.from_owner, e.to_owner
         FROM item_assignment_events e
         JOIN items i ON i.id = e.item_id
         WHERE i.project_id = ?1 AND i.deleted_at IS NULL AND e.created_at >= ?2
         ORDER BY e.item_id, e.created_at",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id, since], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut stats: Vec<HandoffStat> = Vec::new();
    for row in rows {
        let (item_id, from_owner, to_owner) = row?;
        let to_agent = agent_part(&to_owner);
        if stats.last().map(|s| s.item_id.as_str()) != Some(item_id.as_str()) {
            stats.push(HandoffStat {
                item_id,
                handoffs: 0,
                owners: Vec::new(),
            });
        }
        let stat = stats.last_mut().expect("pushed above");
        if from_owner
            .as_deref()
            .is_some_and(|f| agent_part(f) != to_agent)
        {
            stat.handoffs += 1;
        }
        if let Some(from) = from_owner.as_deref().map(agent_part)
            && !stat.owners.contains(&from)
        {
            stat.owners.push(from);
        }
        if !stat.owners.contains(&to_agent) {
            stat.owners.push(to_agent);
        }
    }
    stats.retain(|s| s.handoffs >= 1);
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{item, project, state, workspace};

    fn seed() -> (rusqlite::Connection, String, String) {
        let conn = crate::db::open_in_memory().unwrap();
        let ws = workspace::create(
            &conn,
            workspace::CreateWorkspace {
                name: "W".into(),
                slug: "w".into(),
                item_label: None,
                owner_agent: None,
            },
        )
        .unwrap();
        let proj = project::create(
            &conn,
            project::CreateProject {
                workspace_id: ws.id,
                name: "P".into(),
                identifier: "P".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let backlog = state::list_by_project(&conn, &proj.id)
            .unwrap()
            .into_iter()
            .find(|s| s.group_name == "backlog")
            .unwrap();
        (conn, proj.id, backlog.id)
    }

    fn make_item(conn: &rusqlite::Connection, pid: &str, sid: &str) -> item::Item {
        item::create(
            conn,
            item::CreateItem {
                project_id: pid.to_string(),
                state_id: sid.to_string(),
                name: "I".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: Vec::new(),
                assignee_ids: Vec::new(),
                dependency_ids: Vec::new(),
                start_date: None,
                due_date: None,
            },
        )
        .unwrap()
    }

    fn assign(conn: &rusqlite::Connection, id: &str, agent: &str) {
        item::update(
            conn,
            id,
            item::UpdateItem {
                assignee_agent: Some(agent.to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn update_records_a_transition_only_when_the_assignee_changes() {
        let (conn, pid, sid) = seed();
        let it = make_item(&conn, &pid, &sid);
        assign(&conn, &it.id, "alice"); // None -> alice
        assign(&conn, &it.id, "alice"); // no change, no event
        assign(&conn, &it.id, "bob"); // alice -> bob
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM item_assignment_events WHERE item_id = ?1",
                [&it.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn claim_records_a_transition_through_update() {
        let (conn, pid, sid) = seed();
        let it = make_item(&conn, &pid, &sid);
        item::claim(&conn, &it.id, "alice:1", 1000, 600).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM item_assignment_events WHERE item_id = ?1",
                [&it.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn handoff_stats_count_agent_changes_not_first_assignment_or_instances() {
        let (conn, pid, sid) = seed();
        let it = make_item(&conn, &pid, &sid);
        assign(&conn, &it.id, "alice"); // first assignment — not a handoff
        assign(&conn, &it.id, "alice:2"); // same agent, other instance — not a handoff
        assign(&conn, &it.id, "bob"); // handoff 1
        assign(&conn, &it.id, "carol"); // handoff 2
        let stats = handoff_stats_since(&conn, &pid, 0).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].handoffs, 2);
        assert_eq!(stats[0].owners, vec!["alice", "bob", "carol"]);
    }

    #[test]
    fn handoff_stats_respect_the_since_cutoff_and_skip_no_handoff_items() {
        let (conn, pid, sid) = seed();
        let it = make_item(&conn, &pid, &sid);
        assign(&conn, &it.id, "alice"); // only a first assignment
        assert!(handoff_stats_since(&conn, &pid, 0).unwrap().is_empty());
        assign(&conn, &it.id, "bob");
        let far_future = now() + 10_000;
        assert!(
            handoff_stats_since(&conn, &pid, far_future)
                .unwrap()
                .is_empty()
        );
    }
}
