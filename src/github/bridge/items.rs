//! Local-item side of the bridge.
//!
//! The issue linkage lives in the typed `external_source`/`external_id`
//! columns the backend already has (the same mechanism projects use for their
//! repo key), NOT in metadata JSON. Only `github_last_hash` — mutable sync
//! bookkeeping, not identity — goes in `metadata`.

use agentflare_backend::item::Item;

#[allow(dead_code)] // consumer arrives in a later bridge task
pub const EXTERNAL_SOURCE: &str = "github";
#[allow(dead_code)] // consumer arrives in a later bridge task
const LAST_HASH_KEY: &str = "github_last_hash";

/// The item linked to `number`, if this instance tracks it.
///
/// A full scan rather than an indexed lookup: the backend exposes no
/// by-external-id query, and at this project's item volume a scan is
/// sub-millisecond (the same reasoning `item_health` documents).
#[allow(dead_code)] // consumer arrives in a later bridge task
pub fn find_by_issue(conn: &rusqlite::Connection, project_id: &str, number: u64) -> Option<Item> {
    let wanted = number.to_string();
    agentflare_backend::item::list_by_project(conn, project_id)
        .ok()?
        .into_iter()
        .find(|i| {
            i.external_source.as_deref() == Some(EXTERNAL_SOURCE)
                && i.external_id.as_deref() == Some(wanted.as_str())
        })
}

/// The content hash of this item's last CONFIRMED successful export.
/// `None` (including for malformed metadata) means "never exported", which
/// makes the next tick re-export — the safe direction to fail.
#[allow(dead_code)] // consumer arrives in a later bridge task
pub fn last_hash(item: &Item) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&item.metadata)
        .ok()?
        .get(LAST_HASH_KEY)?
        .as_str()
        .map(str::to_string)
}

/// This item's metadata with `github_last_hash` set, preserving every other
/// key. Malformed existing metadata is replaced rather than propagated.
#[allow(dead_code)] // consumer arrives in a later bridge task
pub fn with_last_hash(item: &Item, hash: &str) -> String {
    let mut v = serde_json::from_str::<serde_json::Value>(&item.metadata)
        .ok()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    v[LAST_HASH_KEY] = serde_json::Value::String(hash.to_string());
    v.to_string()
}

/// First state in `group` (e.g. `backlog`, `started`, `completed`).
#[allow(dead_code)] // consumer arrives in a later bridge task
pub fn state_id_for_group(
    conn: &rusqlite::Connection,
    project_id: &str,
    group: &str,
) -> Option<String> {
    agentflare_backend::state::list_by_project(conn, project_id)
        .ok()?
        .into_iter()
        .find(|s| s.group_name == group)
        .map(|s| s.id)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use agentflare_backend::item::{CreateItem, Item};

    fn item_with_metadata(metadata: &str) -> Item {
        Item {
            id: "i1".into(),
            project_id: "p1".into(),
            state_id: "s1".into(),
            name: "n".into(),
            description: String::new(),
            priority: "none".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id: 1,
            sort_order: 1.0,
            started_at: None,
            completed_at: None,
            archived_at: None,
            external_source: Some(EXTERNAL_SOURCE.into()),
            external_id: Some("42".into()),
            metadata: metadata.into(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
        }
    }

    #[test]
    fn last_hash_reads_the_metadata_key() {
        let i = item_with_metadata(r#"{"github_last_hash":"abc123"}"#);
        assert_eq!(last_hash(&i).as_deref(), Some("abc123"));
    }

    #[test]
    fn last_hash_is_none_for_absent_empty_or_malformed_metadata() {
        assert!(last_hash(&item_with_metadata("")).is_none());
        assert!(last_hash(&item_with_metadata("{}")).is_none());
        assert!(last_hash(&item_with_metadata("not json")).is_none());
        assert!(last_hash(&item_with_metadata(r#"{"other":1}"#)).is_none());
    }

    #[test]
    fn with_last_hash_sets_the_key_and_preserves_other_fields() {
        let i = item_with_metadata(r#"{"size":"M"}"#);
        let updated = with_last_hash(&i, "deadbeef");
        let v: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(v["github_last_hash"], "deadbeef");
        assert_eq!(v["size"], "M", "unrelated metadata must survive");
    }

    #[test]
    fn with_last_hash_recovers_from_malformed_metadata() {
        let i = item_with_metadata("not json");
        let v: serde_json::Value = serde_json::from_str(&with_last_hash(&i, "x")).unwrap();
        assert_eq!(v["github_last_hash"], "x");
    }

    // --- DB-backed ---

    /// Shared with `tick.rs` (Task 6) — one fixture, not two.
    pub(crate) fn tests_support_db() -> (rusqlite::Connection, String) {
        let conn = agentflare_backend::open_in_memory().unwrap();
        let ws = agentflare_backend::workspace::create(
            &conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "w".into(),
                slug: "w".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        // `project::create` runs `state::seed_defaults`, so the six default
        // states (backlog/unstarted/started/completed/cancelled/triage) exist.
        let project = agentflare_backend::project::create(
            &conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id.clone(),
                name: "p".into(),
                identifier: "P".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        (conn, project.id)
    }

    fn db() -> (rusqlite::Connection, String) {
        tests_support_db()
    }

    #[test]
    fn find_by_issue_matches_only_the_github_linked_item() {
        let (conn, project_id) = db();
        let state = state_id_for_group(&conn, &project_id, "backlog").unwrap();

        agentflare_backend::item::create(
            &conn,
            CreateItem {
                project_id: project_id.clone(),
                state_id: state.clone(),
                name: "linked".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: Some(EXTERNAL_SOURCE.into()),
                external_id: Some("42".into()),
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap();

        assert_eq!(
            find_by_issue(&conn, &project_id, 42).unwrap().name,
            "linked"
        );
        assert!(find_by_issue(&conn, &project_id, 43).is_none());
    }

    #[test]
    fn state_id_for_group_finds_the_default_groups() {
        let (conn, project_id) = db();
        assert!(state_id_for_group(&conn, &project_id, "backlog").is_some());
        assert!(state_id_for_group(&conn, &project_id, "completed").is_some());
        assert!(state_id_for_group(&conn, &project_id, "nonsense").is_none());
    }
}
