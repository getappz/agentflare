//! Durable goal state, carried as `metadata.goal` on an `item`. A goal is
//! just an item whose metadata has this shape; its children (via
//! `parent_id`) are its todos.

use super::lifecycle::GoalLifecycle;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct GoalScope {
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub disallowed_actions: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GoalMetadata {
    pub objective: String,
    #[serde(default)]
    pub scope: GoalScope,
    #[serde(default)]
    pub quota_mode: String,
    pub lifecycle: GoalLifecycle,
    #[serde(default)]
    pub consecutive_self_repairs: u32,
}

/// Reads `metadata.goal` out of a raw item-metadata JSON string. `Ok(None)`
/// means "not a goal" (no `goal` key) — that is the common case for an
/// ordinary todo. `Err` means the key IS present but doesn't parse, which
/// callers must treat as fail-closed, not as "no goal."
pub fn parse_goal_metadata(metadata_json: &str) -> Result<Option<GoalMetadata>, String> {
    let value: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|e| format!("item metadata is not valid json: {e}"))?;
    match value.get("goal") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(goal_value) => serde_json::from_value(goal_value.clone())
            .map(Some)
            .map_err(|e| format!("malformed goal metadata: {e}")),
    }
}

/// Writes `goal` back into the item's metadata, preserving any other keys
/// already there (an item's metadata is a shared JSON object; a goal is
/// only ever one key inside it).
pub fn save_goal_metadata(
    conn: &rusqlite::Connection,
    goal_item_id: &str,
    goal: &GoalMetadata,
) -> Result<(), String> {
    let item = agentflare_backend::item::get(conn, goal_item_id)
        .map_err(|e| format!("cannot load goal item {goal_item_id}: {e}"))?;
    let mut value: serde_json::Value =
        serde_json::from_str(&item.metadata).unwrap_or_else(|_| serde_json::json!({}));
    value["goal"] =
        serde_json::to_value(goal).map_err(|e| format!("goal metadata does not serialize: {e}"))?;
    let updated =
        serde_json::to_string(&value).map_err(|e| format!("metadata does not serialize: {e}"))?;
    agentflare_backend::item::update(
        conn,
        goal_item_id,
        agentflare_backend::item::UpdateItem {
            metadata: Some(updated),
            ..Default::default()
        },
    )
    .map_err(|e| format!("cannot save goal metadata on {goal_item_id}: {e}"))?;
    Ok(())
}

/// Walks `item`'s `parent_id` chain (including `item` itself) looking for
/// the nearest ancestor whose metadata carries a `goal`. Returns that
/// ancestor `Item` alongside its parsed `GoalMetadata`. `Ok(None)` means
/// this item has no goal anywhere above it — the normal case for
/// ungrouped work, which callers must treat identically to before this
/// module existed.
pub fn find_goal_ancestor(
    conn: &rusqlite::Connection,
    item: &agentflare_backend::item::Item,
) -> Result<Option<(agentflare_backend::item::Item, GoalMetadata)>, String> {
    let mut current = item.clone();
    loop {
        if let Some(goal) = parse_goal_metadata(&current.metadata)? {
            return Ok(Some((current, goal)));
        }
        let Some(parent_id) = current.parent_id.clone() else {
            return Ok(None);
        };
        current = agentflare_backend::item::get(conn, &parent_id)
            .map_err(|e| format!("cannot load parent {parent_id} of item {}: {e}", item.id))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::lifecycle::GoalLifecycle;

    fn test_conn() -> rusqlite::Connection {
        agentflare_backend::db::open_in_memory().unwrap()
    }

    fn seed_project(conn: &rusqlite::Connection) -> (String, String) {
        let workspace = agentflare_backend::workspace::create(
            conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "ws".into(),
                slug: "ws".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let project = agentflare_backend::project::create(
            conn,
            agentflare_backend::project::CreateProject {
                workspace_id: workspace.id.clone(),
                name: "proj".into(),
                identifier: "proj".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
        let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        (project.id, state_id)
    }

    fn make_item(
        conn: &rusqlite::Connection,
        project_id: &str,
        state_id: &str,
        parent_id: Option<&str>,
        metadata: Option<&str>,
    ) -> agentflare_backend::item::Item {
        agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project_id.into(),
                state_id: state_id.into(),
                name: "item".into(),
                description: None,
                priority: None,
                parent_id: parent_id.map(str::to_string),
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: metadata.map(str::to_string),
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap()
    }

    fn sample_goal_json() -> &'static str {
        r#"{"goal":{"objective":"ship it","scope":{"allowed_paths":[],"disallowed_actions":[]},"quota_mode":"default","lifecycle":"active","consecutive_self_repairs":0}}"#
    }

    #[test]
    fn parse_returns_none_for_plain_item_metadata() {
        assert!(parse_goal_metadata("{}").unwrap().is_none());
    }

    #[test]
    fn parse_reads_a_real_goal() {
        let goal = parse_goal_metadata(sample_goal_json()).unwrap().unwrap();
        assert_eq!(goal.objective, "ship it");
        assert_eq!(goal.lifecycle, GoalLifecycle::Active);
        assert_eq!(goal.consecutive_self_repairs, 0);
    }

    #[test]
    fn parse_fails_closed_on_malformed_goal() {
        let err = parse_goal_metadata(r#"{"goal":{"objective":"x"}}"#).unwrap_err();
        assert!(err.contains("malformed"), "{err}");
    }

    #[test]
    fn save_preserves_sibling_metadata_keys() {
        let conn = test_conn();
        let (project_id, state_id) = seed_project(&conn);
        let item = make_item(
            &conn,
            &project_id,
            &state_id,
            None,
            Some(r#"{"unrelated_key":"keep-me"}"#),
        );
        let goal = GoalMetadata {
            objective: "ship it".into(),
            scope: GoalScope::default(),
            quota_mode: "default".into(),
            lifecycle: GoalLifecycle::Active,
            consecutive_self_repairs: 0,
        };
        save_goal_metadata(&conn, &item.id, &goal).unwrap();

        let reloaded = agentflare_backend::item::get(&conn, &item.id).unwrap();
        let value: serde_json::Value = serde_json::from_str(&reloaded.metadata).unwrap();
        assert_eq!(value["unrelated_key"], "keep-me");
        assert_eq!(value["goal"]["objective"], "ship it");
    }

    #[test]
    fn find_goal_ancestor_walks_up_the_parent_chain() {
        let conn = test_conn();
        let (project_id, state_id) = seed_project(&conn);
        let goal_item = make_item(&conn, &project_id, &state_id, None, Some(sample_goal_json()));
        let child = make_item(&conn, &project_id, &state_id, Some(&goal_item.id), None);
        let grandchild = make_item(&conn, &project_id, &state_id, Some(&child.id), None);

        let (found, meta) = find_goal_ancestor(&conn, &grandchild).unwrap().unwrap();
        assert_eq!(found.id, goal_item.id);
        assert_eq!(meta.objective, "ship it");
    }

    #[test]
    fn find_goal_ancestor_returns_none_for_ungrouped_item() {
        let conn = test_conn();
        let (project_id, state_id) = seed_project(&conn);
        let item = make_item(&conn, &project_id, &state_id, None, None);
        assert!(find_goal_ancestor(&conn, &item).unwrap().is_none());
    }
}
