use super::*;

#[test]
fn item_add_relation_duplicate_is_readable_from_either_item_via_list_relations() {
    let (_tmp, s) = harness();
    let a: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("A"))).unwrap()).unwrap();
    let b: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("B"))).unwrap()).unwrap();
    let a_id = a["id"].as_str().unwrap().to_string();
    let b_id = b["id"].as_str().unwrap().to_string();

    let added: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "add_relation".into(),
            id: Some(a_id.clone()),
            related_item_id: Some(b_id.clone()),
            relation_type: Some("duplicate".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(added["added"], true);

    for (from, to) in [(&a_id, &b_id), (&b_id, &a_id)] {
        let listed: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "list_relations".into(),
                id: Some(from.clone()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let relations = listed["relations"].as_array().unwrap();
        assert_eq!(relations.len(), 1, "from {from}: {relations:?}");
        assert_eq!(relations[0]["relation_type"], "duplicate");
        assert_eq!(relations[0]["item_id"], to.as_str());
    }

    s.item(Parameters(ItemRequest {
        action: "remove_relation".into(),
        id: Some(a_id.clone()),
        related_item_id: Some(b_id.clone()),
        relation_type: Some("duplicate".into()),
        ..Default::default()
    }))
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list_relations".into(),
            id: Some(a_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(listed["relations"].as_array().unwrap().is_empty());
}

#[test]
fn item_add_relation_rejects_unknown_relation_type() {
    let (_tmp, s) = harness();
    let a: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("A"))).unwrap()).unwrap();
    let b: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("B"))).unwrap()).unwrap();

    let err = s
        .item(Parameters(ItemRequest {
            action: "add_relation".into(),
            id: Some(a["id"].as_str().unwrap().into()),
            related_item_id: Some(b["id"].as_str().unwrap().into()),
            relation_type: Some("bogus".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

/// Regression test for item #3's spec §3: a confirmed `duplicate` relation
/// must not be read as a blocking dependency by `groom`, and `groom`'s
/// `confirmed_duplicate` flag must reflect it independently of the
/// unconfirmed `possible_duplicates` name-similarity heuristic.
#[test]
fn item_groom_confirmed_duplicate_does_not_affect_blocked_by_or_fanin() {
    let (_tmp, s) = harness();
    let a: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Alpha"))).unwrap()).unwrap();
    let b: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Beta"))).unwrap()).unwrap();
    let a_id = a["id"].as_str().unwrap().to_string();
    let b_id = b["id"].as_str().unwrap().to_string();

    s.item(Parameters(ItemRequest {
        action: "add_relation".into(),
        id: Some(a_id.clone()),
        related_item_id: Some(b_id.clone()),
        relation_type: Some("duplicate".into()),
        ..Default::default()
    }))
    .unwrap();

    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let items = groomed["items"].as_array().unwrap();
    let a_entry = items.iter().find(|i| i["id"] == a_id).unwrap();
    let b_entry = items.iter().find(|i| i["id"] == b_id).unwrap();

    assert_eq!(a_entry["confirmed_duplicate"], true);
    assert_eq!(b_entry["confirmed_duplicate"], true);
    assert!(
        a_entry["blocked_by"].as_array().unwrap().is_empty(),
        "a duplicate relation must never read as a blocking dependency"
    );
    assert!(b_entry["blocked_by"].as_array().unwrap().is_empty());
    assert_eq!(a_entry["depended_on_by_count"], 0);
    assert_eq!(b_entry["depended_on_by_count"], 0);
}

#[test]
fn item_groom_confirmed_duplicate_false_without_a_persisted_relation() {
    let (_tmp, s) = harness();
    serde_json::from_str::<serde_json::Value>(
        &s.item(Parameters(empty_item_create("Solo"))).unwrap(),
    )
    .unwrap();

    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let items = groomed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["confirmed_duplicate"], false);
}
