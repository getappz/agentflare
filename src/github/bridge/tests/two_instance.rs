//! Proves the protocol converges: two instances, two private ledgers, one
//! shared issue. Exactly one may end up holding it.

use crate::github::bridge::claim::{i_hold, resolve_holder};
use crate::github::bridge::marker::{Action, Marker};

const TTL: i64 = 1800;
const NOW: i64 = 1_754_000_000;

fn claim_body(owner: &str, ts: i64) -> String {
    Marker {
        action: Action::Claim,
        owner: owner.to_string(),
        item: "i".into(),
        ts,
        hash: "h".into(),
    }
    .render()
}

#[test]
fn simultaneous_claims_yield_exactly_one_winner() {
    // Both instances posted before either re-read: the shared comment list
    // now contains both claims.
    let comments = vec![
        (1001, claim_body("a:1", NOW)),
        (1002, claim_body("b:2", NOW)),
    ];
    let a = i_hold(&comments, "a:1", NOW, TTL);
    let b = i_hold(&comments, "b:2", NOW, TTL);
    assert!(a ^ b, "exactly one instance may hold the issue");
    assert!(a, "lowest comment id wins");
}

#[test]
fn the_loser_cedes_and_the_winner_is_unaffected() {
    let mut comments = vec![
        (1001, claim_body("a:1", NOW)),
        (1002, claim_body("b:2", NOW)),
    ];
    // b re-verifies, discovers it lost, and cedes.
    assert!(!i_hold(&comments, "b:2", NOW, TTL));
    comments.push((
        1003,
        Marker {
            action: Action::Cede,
            owner: "b:2".into(),
            item: "i".into(),
            ts: NOW,
            hash: String::new(),
        }
        .render(),
    ));
    assert!(i_hold(&comments, "a:1", NOW, TTL), "winner keeps the claim");
    assert!(!i_hold(&comments, "b:2", NOW, TTL));
}

#[test]
fn a_crashed_holder_is_replaced_after_the_ttl_and_not_before() {
    let comments = vec![(1001, claim_body("a:1", NOW))];
    // Within the TTL, b must not steal.
    let inside = NOW + TTL - 1;
    assert_eq!(
        resolve_holder(&comments, inside, TTL).unwrap().marker.owner,
        "a:1"
    );
    // Past it, the issue is free again.
    let outside = NOW + TTL + 1;
    assert!(resolve_holder(&comments, outside, TTL).is_none());
}

#[test]
fn a_reclaim_after_expiry_transfers_ownership_cleanly() {
    let outside = NOW + TTL + 1;
    let comments = vec![
        (1001, claim_body("a:1", NOW)),
        (1002, claim_body("b:2", outside)),
    ];
    assert!(i_hold(&comments, "b:2", outside, TTL));
    assert!(
        !i_hold(&comments, "a:1", outside, TTL),
        "the crashed holder must not still believe it holds"
    );
}

#[test]
fn a_human_comment_between_claims_changes_nothing() {
    let comments = vec![
        (1001, claim_body("a:1", NOW)),
        (1002, "any update on this?".to_string()),
        (1003, claim_body("b:2", NOW)),
    ];
    assert!(i_hold(&comments, "a:1", NOW, TTL));
    assert!(!i_hold(&comments, "b:2", NOW, TTL));
}
