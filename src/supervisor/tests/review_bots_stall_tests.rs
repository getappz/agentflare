//! Autonomous deadlock & stall tests for review bots.
//!
//! - merge commits newer than review bot comments do not suppress repair dispatch
//! - unreviewed commits newer than review bot comments timeout after LIMIT ticks
//!
//! Split from `review_bots_tests.rs` to stay under the repo LOC gate.

use super::review_bots_tests::*;
use super::tests::{seed_in_review_item_with_claim_age, test_mcp, test_queue};
use super::*;
use crate::github::test_support::{MockResponse, MockServer};
use crate::mcp_server::merge_item_metadata;

#[test]
fn unresolved_thread_with_newer_merge_commit_dispatches_repair() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    mcp.with_backend_db(|conn| {
        merge_item_metadata(conn, &item_id, |m| {
            m.insert(REVIEW_NUDGED_HEAD_KEY.into(), "merge123".into());
        })
        .unwrap();
    })
    .unwrap();
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            &threads_page(&[("PRRT_1", false, vec![(11, BOT, MAJOR_BODY.into())])]),
        ),
        MockResponse::json(
            200,
            r#"{"commit":{"committer":{"date":"2026-09-02T00:00:00Z"},"message":"Merge branch 'master'"},"parents":[{"sha":"p1"},{"sha":"p2"}]}"#,
        ),
    ]);
    let state = sweep(
        &server,
        &mcp,
        &queue,
        &item_id,
        Some("merge123"),
        &ReviewBotConfig::default(),
    );
    assert_eq!(
        state.to_dispatch.len(),
        1,
        "merge commit must not suppress repair dispatch"
    );
    assert_eq!(state.blocking, 1);
}

#[test]
fn unresolved_thread_with_newer_commit_times_out_and_dispatches_repair() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    mcp.with_backend_db(|conn| {
        merge_item_metadata(conn, &item_id, |m| {
            m.insert(REVIEW_NUDGED_HEAD_KEY.into(), "head123".into());
        })
        .unwrap();
    })
    .unwrap();

    // Run for LIMIT - 1 ticks: still waiting on the bot
    for _ in 0..crate::supervisor::review_bots::REVIEW_BOT_WAIT_TICK_LIMIT - 1 {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                &threads_page(&[("PRRT_1", false, vec![(11, BOT, MAJOR_BODY.into())])]),
            ),
            MockResponse::json(
                200,
                r#"{"commit":{"committer":{"date":"2026-09-02T00:00:00Z"},"message":"fix typo"},"parents":[{"sha":"p1"}]}"#,
            ),
        ]);
        let state = sweep(
            &server,
            &mcp,
            &queue,
            &item_id,
            Some("head123"),
            &ReviewBotConfig::default(),
        );
        assert!(state.to_dispatch.is_empty(), "waiting on bot review");
    }

    // On tick LIMIT: wait has timed out, must dispatch repair autonomously
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            &threads_page(&[("PRRT_1", false, vec![(11, BOT, MAJOR_BODY.into())])]),
        ),
        MockResponse::json(
            200,
            r#"{"commit":{"committer":{"date":"2026-09-02T00:00:00Z"},"message":"fix typo"},"parents":[{"sha":"p1"}]}"#,
        ),
    ]);
    let state = sweep(
        &server,
        &mcp,
        &queue,
        &item_id,
        Some("head123"),
        &ReviewBotConfig::default(),
    );
    assert_eq!(
        state.to_dispatch.len(),
        1,
        "bot wait timeout must dispatch repair"
    );
}
