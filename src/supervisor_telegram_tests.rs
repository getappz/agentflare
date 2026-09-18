//! Telegram inbound-routing tests split out of `supervisor_tests.rs` when
//! merging item #573's plan-approval-card tests with master's chat-channel
//! tests pushed that file past the frozen LOC-gate limit
//! (`scripts/loc-gate.sh`). Pure move: `use super::*` keeps every helper
//! (`test_mcp`, `handle_telegram_callback`, `handle_chat_message`, ...)
//! available exactly as it was in the parent file.

use super::*;

/// Task #573 Task 6: a Telegram "Approve" tap on a plan-approval card
/// (`callback_data = "approve_plan:{item_id}"`) must route to
/// `mcp.item_approve_plan` and move the item's `plan_status` to
/// `"approved"` -- the inbound half of `notify_plan_approval_gate`'s card,
/// parallel to `parse_approve_callback`'s PR-approval handling below.
#[test]
fn handle_telegram_callback_approves_a_pending_plan() {
    // handle_telegram_callback's ack/clear calls touch the vault (via
    // channels::telegram_token) -- run under an isolated home so this can't
    // read the developer's real vault, same reasoning as the gated-dispatch
    // tests above.
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let item_id = mcp
            .with_backend_db(|conn| {
                let project = mcp.resolve_project(conn).unwrap();
                let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
                let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
                agentflare_backend::item::create(
                    conn,
                    agentflare_backend::item::CreateItem {
                        project_id: project.id.clone(),
                        state_id,
                        name: "Plan awaiting approval".into(),
                        description: None,
                        priority: None,
                        parent_id: None,
                        assignee_agent: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                        metadata: Some(
                            r#"{"plan_required":true,"plan_status":"pending","plan_asset_id":"asset-1"}"#
                                .into(),
                        ),
                        label_ids: vec![],
                        assignee_ids: vec![],
                        dependency_ids: vec![],
                        start_date: None,
                        due_date: None,
                    },
                )
                .unwrap()
                .id
            })
            .unwrap();

        let expected_chat_id = "424242";
        let update = serde_json::json!({
            "update_id": 1,
            "callback_query": {
                "id": "cb-1",
                "data": format!("approve_plan:{item_id}"),
                "message": {
                    "message_id": 7,
                    "chat": { "id": expected_chat_id.parse::<i64>().unwrap() },
                },
            },
        });

        handle_telegram_callback(&update, expected_chat_id, &mcp);

        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
        assert_eq!(
            metadata["plan_status"], "approved",
            "the Approve tap must move plan_status to approved: {metadata}"
        );
    });
}

/// A callback whose chat id doesn't match the configured notify chat must
/// not approve the plan -- same defense-in-depth check the PR-approval
/// branch already relies on.
#[test]
fn handle_telegram_callback_ignores_a_plan_approve_from_an_unexpected_chat() {
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let item_id = mcp
            .with_backend_db(|conn| {
                let project = mcp.resolve_project(conn).unwrap();
                let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
                let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
                agentflare_backend::item::create(
                    conn,
                    agentflare_backend::item::CreateItem {
                        project_id: project.id.clone(),
                        state_id,
                        name: "Plan awaiting approval".into(),
                        description: None,
                        priority: None,
                        parent_id: None,
                        assignee_agent: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                        metadata: Some(
                            r#"{"plan_required":true,"plan_status":"pending","plan_asset_id":"asset-1"}"#
                                .into(),
                        ),
                        label_ids: vec![],
                        assignee_ids: vec![],
                        dependency_ids: vec![],
                        start_date: None,
                        due_date: None,
                    },
                )
                .unwrap()
                .id
            })
            .unwrap();

        let update = serde_json::json!({
            "update_id": 1,
            "callback_query": {
                "id": "cb-1",
                "data": format!("approve_plan:{item_id}"),
                "message": {
                    "message_id": 7,
                    "chat": { "id": 999_999 },
                },
            },
        });

        handle_telegram_callback(&update, "424242", &mcp);

        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
        assert_eq!(
            metadata["plan_status"], "pending",
            "a callback from an unexpected chat id must not approve the plan: {metadata}"
        );
    });
}

// -- Telegram inbound routing: characterization tests written before
// merging the chat channel's polling into this same tick (see
// `poll_telegram_approvals`/`handle_telegram_callback`) -- these pin down
// the existing approval-callback behavior so the merge can't silently
// change it. No tests previously covered this path.

#[test]
fn parse_approve_callback_extracts_repo_and_number() {
    let parsed = parse_approve_callback("approve:owner/repo#42");
    assert!(parsed.is_some());
    let (repo, number) = parsed.unwrap();
    assert_eq!(repo.to_string(), "owner/repo");
    assert_eq!(number, 42);
}

#[test]
fn parse_approve_callback_rejects_non_approve_prefix() {
    assert_eq!(parse_approve_callback("deny:owner/repo#42"), None);
}

#[test]
fn parse_approve_callback_rejects_missing_number() {
    assert_eq!(parse_approve_callback("approve:owner/repo"), None);
}

#[test]
fn handle_telegram_callback_ignores_update_with_no_callback_query() {
    // A plain message update -- must not panic and must not attempt any
    // GitHub call (no network in this test process, so a GitHub attempt
    // would hang/fail rather than silently succeed).
    let update = serde_json::json!({
        "update_id": 1,
        "message": { "chat": { "id": 999 }, "text": "hello" }
    });
    let mcp = test_mcp();
    handle_telegram_callback(&update, "999", &mcp);
}

#[test]
fn handle_telegram_callback_ignores_callback_from_wrong_chat() {
    let update = serde_json::json!({
        "update_id": 2,
        "callback_query": {
            "id": "cb1",
            "data": "approve:owner/repo#1",
            "message": { "message_id": 5, "chat": { "id": 111 } }
        }
    });
    // expected_chat_id is "999", update is from chat 111 -- must return
    // early (before any GitHub call) rather than panic.
    let mcp = test_mcp();
    handle_telegram_callback(&update, "999", &mcp);
}

#[test]
fn handle_telegram_callback_ignores_malformed_callback_query() {
    let update = serde_json::json!({
        "update_id": 3,
        "callback_query": { "id": "cb1" } // missing "data"
    });
    let mcp = test_mcp();
    handle_telegram_callback(&update, "999", &mcp);
}

// -- `handle_chat_message`: the merged poll's other branch (see
// `poll_telegram_approvals`). Only exercises the authorization/shape gate
// here -- once past it, dispatch_message hands off to chat_channel, which
// has its own test coverage for command parsing.

#[test]
fn handle_chat_message_ignores_message_from_wrong_chat() {
    let message = serde_json::json!({ "chat": { "id": 111 }, "text": "hi" });
    let mcp = std::sync::Arc::new(test_mcp());
    // expected_chat_id is "999", message is from chat 111 -- must return
    // without dispatching anything (no panic, no network/db touch).
    handle_chat_message(&message, "999", &mcp, 1);
}

#[test]
fn handle_chat_message_ignores_message_with_no_text() {
    let message = serde_json::json!({ "chat": { "id": 999 } });
    let mcp = std::sync::Arc::new(test_mcp());
    handle_chat_message(&message, "999", &mcp, 2);
}

#[test]
fn handle_chat_message_ignores_whitespace_only_text() {
    let message = serde_json::json!({ "chat": { "id": 999 }, "text": "   " });
    let mcp = std::sync::Arc::new(test_mcp());
    handle_chat_message(&message, "999", &mcp, 3);
}

#[test]
fn handle_chat_message_ignores_message_with_no_chat() {
    let message = serde_json::json!({ "text": "hi" });
    let mcp = std::sync::Arc::new(test_mcp());
    handle_chat_message(&message, "999", &mcp, 4);
}

// -- Offset watermark: safe_offset_to_persist / mark_in_flight / settle
// (see poll_telegram_approvals). These back the fix for the exact gap
// CodeRabbit's review flagged on this PR -- a free-text turn's offset must
// not be confirmed before the turn itself finishes, and a later
// synchronously-settled update must not drag the offset past an earlier
// one that's still in flight.

#[test]
fn safe_offset_to_persist_is_ceiling_when_nothing_in_flight() {
    let in_flight = std::collections::BTreeSet::new();
    assert_eq!(safe_offset_to_persist(50, &in_flight), 50);
}

#[test]
fn safe_offset_to_persist_caps_below_earliest_in_flight_update() {
    let in_flight = std::collections::BTreeSet::from([30, 45]);
    assert_eq!(safe_offset_to_persist(50, &in_flight), 29);
}

#[test]
fn safe_offset_to_persist_withholds_a_later_offset_while_an_earlier_one_is_still_in_flight() {
    // The scenario the review flagged: update N (free text, still running)
    // must not be silently confirmed just because update N+1 (e.g. a slash
    // command) already finished synchronously and isn't itself in the set.
    let in_flight = std::collections::BTreeSet::from([10]);
    assert_eq!(safe_offset_to_persist(12, &in_flight), 9);
}

#[test]
fn safe_offset_to_persist_never_exceeds_ceiling() {
    // Shouldn't happen in practice (an in-flight offset can't exceed the
    // ceiling, which tracks every offset ever seen) but the result must
    // stay bounded even if it somehow did.
    let in_flight = std::collections::BTreeSet::from([999]);
    assert_eq!(safe_offset_to_persist(50, &in_flight), 50);
}

#[test]
fn mark_in_flight_then_settle_round_trips_through_the_shared_set() {
    // High sentinel value: IN_FLIGHT_UPDATE_OFFSETS is a real process-wide
    // static shared with other tests under cargo test's parallel execution.
    const SENTINEL: i64 = 900_001;
    mark_in_flight(SENTINEL);
    assert!(
        IN_FLIGHT_UPDATE_OFFSETS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&SENTINEL),
        "mark_in_flight must record the offset as in flight"
    );
    settle(SENTINEL);
    assert!(
        !IN_FLIGHT_UPDATE_OFFSETS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&SENTINEL),
        "settle must remove it once handling is done"
    );
}
