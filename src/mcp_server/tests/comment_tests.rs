use super::*;

#[test]
fn item_comment_create_and_list_roundtrip() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let comment: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some("Hello, world!".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(comment["body"], "Hello, world!");
    assert!(comment["author_agent"].as_str().unwrap().contains(':'));

    let comments: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "list".into(),
            item_id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let arr = comments.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["body"], "Hello, world!");
}

/// #375: `comment` was the one id-taking tool that never resolved a
/// sequence_id, so `item_id: "327"` reached the INSERT and came back as a raw
/// "FOREIGN KEY constraint failed".
#[test]
fn item_comment_accepts_sequence_id_bare_and_hash_prefixed() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let seq = created["sequence_id"].as_i64().unwrap();

    for (id, body) in [
        (seq.to_string(), "bare numeric"),
        (format!("#{seq}"), "hash prefixed"),
        (item_id.clone(), "uuid"),
    ] {
        let comment: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "create".into(),
                item_id: Some(id.clone()),
                body: Some(body.into()),
                ..Default::default()
            }))
            .unwrap_or_else(|e| panic!("create with item_id {id:?} failed: {e:?}")),
        )
        .unwrap();
        // Whichever spelling went in, the comment must hang off the UUID.
        assert_eq!(comment["item_id"], serde_json::json!(item_id));
    }

    // `list` resolves the same three spellings and sees all three comments.
    for id in [seq.to_string(), format!("#{seq}"), item_id.clone()] {
        let comments: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "list".into(),
                item_id: Some(id.clone()),
                ..Default::default()
            }))
            .unwrap_or_else(|e| panic!("list with item_id {id:?} failed: {e:?}")),
        )
        .unwrap();
        assert_eq!(comments.as_array().unwrap().len(), 3, "item_id {id:?}");
    }
}

#[test]
fn item_comment_rejects_an_unresolvable_item_id_naming_it() {
    let (_tmp, s) = harness();
    // Seeds the project so resolution gets as far as the lookup itself.
    s.item(Parameters(empty_item_create("Test"))).unwrap();

    for action in ["create", "list"] {
        for bogus in ["4242", "#4242", "no-such-item-uuid"] {
            let err = s
                .comment(Parameters(CommentRequest {
                    action: action.into(),
                    item_id: Some(bogus.into()),
                    body: Some("hi".into()),
                    ..Default::default()
                }))
                .unwrap_err();
            assert_eq!(
                err.code,
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "{action} {bogus}"
            );
            assert!(
                err.message.contains("4242") || err.message.contains("no-such-item-uuid"),
                "{action} {bogus}: error must name the id, got {:?}",
                err.message
            );
        }
    }
}

#[test]
fn item_comment_rejects_empty_body() {
    let (_tmp, s) = harness();
    let err = s
        .comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some("item-1".into()),
            body: Some("".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_comment_edit_succeeds_when_latest_and_own_and_unclaimed_by_other() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let comment: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some("original".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let comment_id = comment["id"].as_str().unwrap().to_string();

    let updated: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some(comment_id.clone()),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["body"], "edited");
}

#[test]
fn item_comment_edit_rejected_when_comment_not_found() {
    let (_tmp, s) = harness();
    let err = s
        .comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some("nonexistent".into()),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_comment_edit_rejected_when_different_agent() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let comment_id = s
        .with_backend_db(|conn| {
            agentflare_backend::comment::create(conn, &item_id, "someone-else:1", "not mine")
                .unwrap()
                .id
        })
        .unwrap();

    let err = s
        .comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some(comment_id),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("own comments"));
}

#[test]
fn item_comment_edit_succeeds_across_sessions_of_same_agent() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    // Same agent, different session instance — e.g. a prior CLI
    // invocation, or an MCP server process that has since restarted.
    let agent = crate::claims::agent_of(&crate::claims::owner_id()).to_string();
    let earlier_session_author = format!("{agent}:some-earlier-session");

    let comment_id = s
        .with_backend_db(|conn| {
            agentflare_backend::comment::create(
                conn,
                &item_id,
                &earlier_session_author,
                "mine, from an earlier session",
            )
            .unwrap()
            .id
        })
        .unwrap();

    let updated: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some(comment_id),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["body"], "edited");
}

#[test]
fn item_comment_edit_uses_id_tiebreak_when_timestamps_collide() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let first: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some("first".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let first_id = first["id"].as_str().unwrap().to_string();

    let second: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id),
            body: Some("second".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let second_id = second["id"].as_str().unwrap().to_string();

    // ID tiebreak: nanoid is random, so determine "latest" at runtime.
    let (lower_id, higher_id) = if first_id > second_id {
        (second_id, first_id)
    } else {
        (first_id, second_id)
    };

    // Force both comments onto the same second-resolution timestamp.
    s.with_backend_db(|conn| {
        conn.execute(
            "UPDATE item_comments SET created_at = 1000, updated_at = 1000",
            [],
        )
        .unwrap();
    })
    .unwrap();

    let err = s
        .comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some(lower_id),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    let updated: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some(higher_id),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["body"], "edited");
}

#[test]
fn item_comment_delete_succeeds_when_latest_and_own() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let comment: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id),
            body: Some("delete-me".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let comment_id = comment["id"].as_str().unwrap().to_string();

    let result: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "delete".into(),
            id: Some(comment_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["deleted"], true);
}
