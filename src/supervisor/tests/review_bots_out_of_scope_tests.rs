//! Item #766: recorded `out_of_scope` / `not_valid` review results neither
//! block the merge nor dispatch a repair job. Split from
//! `review_bots_tests.rs` to stay under the repo LOC gate.

use super::review_bots_tests::*;
use super::tests::{seed_in_review_item_with_claim_age, test_mcp, test_queue};
use super::*;
use crate::github::test_support::{MockResponse, MockServer};

#[test]
fn sweep_does_not_block_or_dispatch_a_recorded_out_of_scope_result_whose_reply_failed() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    record_result(
        &mcp,
        &item_id,
        "PRRT_1",
        &ReviewResult {
            round: 1,
            outcome: ReviewOutcome::OutOfScope,
            sha: None,
            note: "tracked in the follow-up item".into(),
            test: None,
        },
    );
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            &threads_page(&[("PRRT_1", false, vec![(11, BOT, MAJOR_BODY.into())])]),
        ),
        MockResponse::json(502, r#"{"message":"bad gateway"}"#),
    ]);
    let state = sweep(
        &server,
        &mcp,
        &queue,
        &item_id,
        None,
        &ReviewBotConfig::default(),
    );
    assert_eq!(state.replied, 0, "the reply failed");
    assert!(state.to_dispatch.is_empty(), "nothing for an agent to fix");
    assert_eq!(state.blocking, 0);
    assert!(!state.blocks_merge());
}

#[test]
fn sweep_still_blocks_and_dispatches_an_unrecorded_thread() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let server = MockServer::start(vec![MockResponse::json(
        200,
        &threads_page(&[("PRRT_1", false, vec![(11, BOT, MAJOR_BODY.into())])]),
    )]);
    let state = sweep(
        &server,
        &mcp,
        &queue,
        &item_id,
        None,
        &ReviewBotConfig::default(),
    );
    assert_eq!(state.to_dispatch.len(), 1);
    assert!(state.blocks_merge());
}

#[test]
fn classify_threads_reopens_an_out_of_scope_reply_the_bot_pushes_back_on() {
    let cfg = ReviewBotConfig::default();
    let threads = vec![thread(
        "PRRT_1",
        false,
        vec![
            comment(1, BOT, MAJOR_BODY, "t1"),
            comment(
                2,
                "me",
                &our_reply("PRRT_1", 1, ReviewOutcome::OutOfScope, ""),
                "t2",
            ),
            comment(
                3,
                BOT,
                "No, this is a real bug and must be fixed here.",
                "t3",
            ),
        ],
    )];
    let meta = replied_meta(&[("PRRT_1", 1)]);
    let findings = classify_threads(&threads, &cfg, &meta);
    assert!(matches!(
        findings[0].status,
        ThreadStatus::Actionable { next_round: 2 }
    ));
    assert!(findings[0].blocks_merge());
}
