//! Item #771: `item action=update metadata={...}` merges into the stored
//! metadata object instead of replacing the column wholesale. A caller that
//! sends only the keys it cares about (`{"task_type":"bugfix"}`,
//! `{"plan_required":true}`) must not wipe `size`/`slice`/`pr`/`plan_*`.
//! Explicit JSON `null` is the one way to delete a key.

use super::*;

fn item_with_metadata(s: &AgentflareMcp, metadata: serde_json::Value) -> String {
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Merge target".into()),
            metadata: Some(metadata),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    created["id"].as_str().unwrap().to_string()
}

fn update_metadata(
    s: &AgentflareMcp,
    id: &str,
    patch: serde_json::Value,
) -> Result<serde_json::Value, ErrorData> {
    let updated = s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(id.to_string()),
        metadata: Some(patch),
        ..Default::default()
    }))?;
    let updated: serde_json::Value = serde_json::from_str(&updated).unwrap();
    Ok(serde_json::from_str(updated["metadata"].as_str().unwrap()).unwrap())
}

#[test]
fn item_update_metadata_merges_into_existing_keys() {
    let (tmp, s) = harness();
    let id = item_with_metadata(&s, serde_json::json!({"size": "M", "slice": 4}));

    let metadata = update_metadata(&s, &id, serde_json::json!({"task_type": "refactor"})).unwrap();

    assert_eq!(metadata["size"], "M", "size must survive: {metadata}");
    assert_eq!(metadata["slice"], 4, "slice must survive: {metadata}");
    assert_eq!(metadata["task_type"], "refactor");

    // And the row itself, not just the echoed response.
    let conn = backend_conn(&tmp);
    let stored: serde_json::Value =
        serde_json::from_str(&agentflare_backend::item::get(&conn, &id).unwrap().metadata).unwrap();
    assert_eq!(stored, metadata);
}

#[test]
fn item_update_metadata_null_deletes_key() {
    let (_tmp, s) = harness();
    let id = item_with_metadata(
        &s,
        serde_json::json!({
            "size": "M", "slice": 4, "optional": null,
            "pr": {"number": 7, "branch": "task/7"},
        }),
    );

    let metadata = update_metadata(&s, &id, serde_json::json!({"size": null})).unwrap();

    assert!(
        metadata.get("size").is_none(),
        "size sent as null must be removed, not stored as null: {metadata}"
    );
    assert_eq!(metadata["slice"], 4);
    assert_eq!(metadata["pr"]["number"], 7);
    assert_eq!(metadata.get("optional"), Some(&serde_json::Value::Null));

    let metadata = update_metadata(&s, &id, serde_json::json!({"pr": null})).unwrap();
    assert!(metadata.get("pr").is_none());
    assert_eq!(metadata["slice"], 4);
}

#[test]
fn item_update_metadata_overwrites_same_key() {
    let (_tmp, s) = harness();
    let id = item_with_metadata(&s, serde_json::json!({"size": "M", "slice": 4}));

    let metadata = update_metadata(&s, &id, serde_json::json!({"size": "L"})).unwrap();

    assert_eq!(metadata["size"], "L");
    assert_eq!(metadata["slice"], 4);
}

/// Successive partial updates from different callers (the live PM-triage
/// shape: groom sets `size`, a later update sets `plan_required`) must
/// compose instead of the last writer winning.
#[test]
fn item_update_metadata_successive_partial_updates_compose() {
    let (_tmp, s) = harness();
    let id = item_with_metadata(&s, serde_json::json!({}));

    update_metadata(&s, &id, serde_json::json!({"size": "S"})).unwrap();
    update_metadata(&s, &id, serde_json::json!({"task_type": "bugfix"})).unwrap();
    let metadata = update_metadata(&s, &id, serde_json::json!({"slice": 2})).unwrap();

    assert_eq!(metadata["size"], "S", "{metadata}");
    assert_eq!(metadata["task_type"], "bugfix", "{metadata}");
    assert_eq!(metadata["slice"], 2, "{metadata}");
}

/// The double-encoded shape (`metadata` arriving as a JSON *string*) still
/// merges like an object, matching `metadata_to_json_string`'s unwrap.
#[test]
fn item_update_metadata_merges_a_double_encoded_patch() {
    let (_tmp, s) = harness();
    let id = item_with_metadata(&s, serde_json::json!({"size": "M"}));

    let metadata = update_metadata(
        &s,
        &id,
        serde_json::Value::String(serde_json::json!({"task_type": "chore"}).to_string()),
    )
    .unwrap();

    assert_eq!(metadata["size"], "M");
    assert_eq!(metadata["task_type"], "chore");
}

/// Legacy rows hold the object double-encoded as a JSON string; a partial
/// update must still keep their existing keys.
#[test]
fn item_update_metadata_merges_into_a_double_encoded_stored_object() {
    let (tmp, s) = harness();
    let id = item_with_metadata(&s, serde_json::json!({}));
    {
        let conn = backend_conn(&tmp);
        agentflare_backend::item::update(
            &conn,
            &id,
            agentflare_backend::item::UpdateItem {
                metadata: Some(serde_json::json!(r#"{"size":"S"}"#).to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    }

    let metadata = update_metadata(&s, &id, serde_json::json!({"task_type": "chore"})).unwrap();

    assert_eq!(metadata["size"], "S", "legacy key must survive: {metadata}");
    assert_eq!(metadata["task_type"], "chore");
}

/// A non-object patch can't be merged; it is rejected instead of silently
/// replacing the stored object (the old behaviour) or being ignored.
#[test]
fn item_update_metadata_rejects_a_non_object_patch() {
    let (tmp, s) = harness();
    let id = item_with_metadata(&s, serde_json::json!({"size": "M"}));

    let err = update_metadata(&s, &id, serde_json::json!(["size", "M"])).unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    let conn = backend_conn(&tmp);
    let stored: serde_json::Value =
        serde_json::from_str(&agentflare_backend::item::get(&conn, &id).unwrap().metadata).unwrap();
    assert_eq!(
        stored["size"], "M",
        "a rejected patch must not touch the row"
    );
}

/// The merge runs BEFORE `restore_plan_transition_fields`: a partial update
/// can neither drop nor forge the approval record, and sending
/// `plan_status: null` does not clear a recorded approval either.
#[test]
fn item_update_metadata_merge_keeps_plan_transition_fields_pinned() {
    let (tmp, s) = harness();
    let id = item_with_metadata(&s, serde_json::json!({"size": "M"}));
    {
        let conn = backend_conn(&tmp);
        let current = agentflare_backend::item::get(&conn, &id).unwrap().metadata;
        let approved = agentflare_backend::item::plan_gate::merge_metadata_patch(
            &current,
            serde_json::json!({
                "plan_required": true,
                "plan_asset_id": "asset-1",
                "plan_status": "approved",
                "plan_approved_by": "human",
                "plan_approved_at": 5,
            }),
        );
        agentflare_backend::item::update(
            &conn,
            &id,
            agentflare_backend::item::UpdateItem {
                metadata: Some(approved),
                ..Default::default()
            },
        )
        .unwrap();
    }

    let metadata = update_metadata(
        &s,
        &id,
        serde_json::json!({"task_type": "bugfix", "plan_status": null, "plan_approved_by": "me"}),
    )
    .unwrap();

    assert_eq!(metadata["task_type"], "bugfix");
    assert_eq!(metadata["size"], "M");
    assert_eq!(metadata["plan_status"], "approved", "{metadata}");
    assert_eq!(metadata["plan_approved_by"], "human", "{metadata}");
    assert_eq!(metadata["plan_approved_at"], 5, "{metadata}");
    assert_eq!(metadata["plan_asset_id"], "asset-1", "{metadata}");
}
