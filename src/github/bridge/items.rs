//! Local-item side of the bridge.
//!
//! The issue linkage lives in the typed `external_source`/`external_id`
//! columns the backend already has (the same mechanism projects use for their
//! repo key), NOT in metadata JSON. Only `github_last_hash` — mutable sync
//! bookkeeping, not identity — goes in `metadata`.

use agentflare_backend::item::Item;

pub const EXTERNAL_SOURCE: &str = "github";

const LAST_HASH_KEY: &str = "github_last_hash";

/// The item linked to `number`, if this instance tracks it.
///
/// A full scan rather than an indexed lookup: the backend exposes no
/// by-external-id query, and at this project's item volume a scan is
/// sub-millisecond (the same reasoning `item_health` documents).
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
pub fn last_hash(item: &Item) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&item.metadata)
        .ok()?
        .get(LAST_HASH_KEY)?
        .as_str()
        .map(str::to_string)
}

/// This item's metadata with `github_last_hash` set, preserving every other
/// key. Malformed existing metadata is replaced rather than propagated.
pub fn with_last_hash(item: &Item, hash: &str) -> String {
    let mut v = serde_json::from_str::<serde_json::Value>(&item.metadata)
        .ok()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    v[LAST_HASH_KEY] = serde_json::Value::String(hash.to_string());
    v.to_string()
}

/// This item's metadata with `github_last_hash` REMOVED, preserving every
/// other key. Used to roll the export latch back after a failed remote write
/// without disturbing metadata anyone else owns.
pub fn without_last_hash(item: &Item) -> String {
    let mut v = serde_json::from_str::<serde_json::Value>(&item.metadata)
        .ok()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.remove(LAST_HASH_KEY);
    }
    v.to_string()
}

/// First state in `group` (e.g. `backlog`, `started`, `completed`).
pub fn state_id_for_group(
    conn: &rusqlite::Connection,
    project_id: &str,
    group: &str,
) -> Option<String> {
    agentflare_backend::state::first_in_group(conn, project_id, group)
        .ok()
        .map(|s| s.id)
}

/// Whether `item` is still actively held, as opposed to locally cancelled (or
/// completed) but still linked to its issue.
///
/// Moving an item to the `cancelled` state group does not set
/// `completed_at` — only the `started`/`completed` groups touch timestamps
/// (`agentflare_backend::item::update_state`) — so `completed_at.is_none()`
/// alone cannot distinguish "actively held" from "ceded, still linked".
/// This checks the item's actual state group instead, which is the only
/// reliable signal. Unresolvable state (e.g. deleted) fails open as active,
/// matching the pre-existing behavior of treating every linked item as live.
pub fn is_active(conn: &rusqlite::Connection, item: &Item) -> bool {
    agentflare_backend::state::get(conn, &item.state_id)
        .map(|s| !matches!(s.group_name.as_str(), "cancelled" | "completed"))
        .unwrap_or(true)
}

/// Whether `item` was CEDED — locally cancelled, but still linked to its
/// issue and therefore re-adoptable if we win the claim again.
///
/// Deliberately narrower than `!is_active`, which is also false for
/// `completed`: a completed item still owes its issue a `done` export (and
/// the close that follows), so treating it as re-claimable would have us
/// re-open work we had just finished. Only `cancelled` is recoverable.
/// Unresolvable state fails closed as NOT ceded, matching `is_active`'s
/// fail-open-as-live direction.
pub fn is_ceded(conn: &rusqlite::Connection, item: &Item) -> bool {
    agentflare_backend::state::get(conn, &item.state_id)
        .map(|s| s.group_name == "cancelled")
        .unwrap_or(false)
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

    #[test]
    fn with_last_hash_recovers_from_valid_json_that_is_not_an_object() {
        for non_object in ["[]", "42", r#""str""#, "null", "true"] {
            let i = item_with_metadata(non_object);
            let updated = with_last_hash(&i, "deadbeef");
            let v: serde_json::Value = serde_json::from_str(&updated)
                .unwrap_or_else(|e| panic!("{non_object} produced invalid JSON: {e}"));
            assert!(
                v.is_object(),
                "{non_object} must be replaced with an object, got {v}"
            );
            assert_eq!(
                v["github_last_hash"], "deadbeef",
                "{non_object} must still carry the new hash"
            );
        }
    }

    #[test]
    fn last_hash_is_none_when_github_last_hash_is_not_a_string() {
        for metadata in [
            r#"{"github_last_hash":42}"#,
            r#"{"github_last_hash":{"nested":true}}"#,
            r#"{"github_last_hash":null}"#,
            r#"{"github_last_hash":[1,2,3]}"#,
        ] {
            let i = item_with_metadata(metadata);
            assert!(
                last_hash(&i).is_none(),
                "{metadata} must yield None, not panic"
            );
        }
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
