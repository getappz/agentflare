//! Cross-instance claim arbitration, decided entirely from an issue's comment
//! list. Two private SQLite ledgers cannot arbitrate between themselves, so
//! GitHub is the one shared medium that can — and this is the only question
//! GitHub is authoritative for.
//!
//! Pure by construction: everything here operates on `(comment_id, body)`
//! pairs, so the whole race is unit-testable with no HTTP.

use crate::github::bridge::marker::{Action, Marker};

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // consumer arrives in a later bridge task
pub struct Holder {
    pub comment_id: u64,
    pub marker: Marker,
}

/// The owner currently holding this issue, if any.
///
/// Lowest comment id wins: ids are monotonic, whereas `created_at` is
/// second-granular and subject to cross-machine clock skew.
#[allow(dead_code)] // consumer arrives in a later bridge task
pub fn resolve_holder(comments: &[(u64, String)], now: i64, ttl_secs: i64) -> Option<Holder> {
    let parsed: Vec<(u64, Marker)> = comments
        .iter()
        .filter_map(|(id, body)| Marker::parse(body).map(|m| (*id, m)))
        .collect();

    // A `done` only ends the issue if its owner previously claimed it (with a
    // lower comment id): markers are hidden HTML comments written only by
    // agents, and the agent that completes work is by construction the one
    // that claimed it, so a `done` from an owner who never claimed is
    // malformed or misdirected and must not permanently end resolution.
    let issue_done = parsed.iter().any(|(done_id, done_m)| {
        done_m.action == Action::Done
            && parsed.iter().any(|(claim_id, claim_m)| {
                claim_m.action == Action::Claim
                    && claim_m.owner == done_m.owner
                    && claim_id < done_id
            })
    });
    if issue_done {
        return None;
    }

    parsed
        .iter()
        .filter(|(_, m)| m.action == Action::Claim)
        .filter(|(claim_id, m)| !is_ceded(*claim_id, &m.owner, &parsed))
        .filter(|(_, m)| {
            latest_ts_for(&m.owner, &parsed).is_some_and(|ts| now.saturating_sub(ts) <= ttl_secs)
        })
        .min_by_key(|(id, _)| *id)
        .map(|(id, m)| Holder {
            comment_id: *id,
            marker: m.clone(),
        })
}

/// A claim is retired only by a LATER `Cede` from the same owner — one whose
/// comment id is higher than the claim's. `Cede` is not a permanent ban: an
/// instance that crashes, cedes defensively, restarts, and posts a fresh
/// `Claim` under the same owner id is a normal recovery path, and that later
/// claim (higher comment id than the cede) must survive.
fn is_ceded(claim_id: u64, owner: &str, parsed: &[(u64, Marker)]) -> bool {
    parsed
        .iter()
        .any(|(cede_id, m)| m.action == Action::Cede && m.owner == owner && *cede_id > claim_id)
}

/// An owner's liveness is its most recent marker of ANY action, so a
/// `progress` heartbeat keeps an old claim alive.
///
/// Returns `None` — never a sentinel — for an owner with no markers at all,
/// so callers express "never seen" without doing arithmetic on a magic
/// value. This can't happen via `resolve_holder` today, whose only caller
/// passes an owner drawn from the parsed list, so `max()` always finds at
/// least the marker that produced it; but returning `i64::MIN` here used to
/// let `now - latest_ts_for(...)` panic on subtract-with-overflow (under the
/// debug overflow checks `cargo test` uses) the moment a future caller
/// checked an owner before it had any marker. `now.saturating_sub(ts)` at
/// the call site is kept too, since a crafted/corrupt marker could in
/// principle carry `ts == i64::MIN` itself.
#[allow(dead_code)] // consumer arrives in a later bridge task
fn latest_ts_for(owner: &str, parsed: &[(u64, Marker)]) -> Option<i64> {
    parsed
        .iter()
        .filter(|(_, m)| m.owner == owner)
        .map(|(_, m)| m.ts)
        .max()
}

/// Whether `me` is the current holder. Re-checked every tick, not trusted
/// once: GitHub comment listing is not instantly consistent, so two instances
/// can each briefly believe they won.
#[allow(dead_code)] // consumer arrives in a later bridge task
pub fn i_hold(comments: &[(u64, String)], me: &str, now: i64, ttl_secs: i64) -> bool {
    resolve_holder(comments, now, ttl_secs).is_some_and(|h| h.marker.owner == me)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::bridge::marker::{Action, Marker};

    const TTL: i64 = 1800;
    const NOW: i64 = 1_754_000_000;

    fn mk(action: Action, owner: &str, ts: i64) -> String {
        Marker {
            action,
            owner: owner.to_string(),
            item: "i".to_string(),
            ts,
            hash: "h".to_string(),
        }
        .render()
    }

    #[test]
    fn no_comments_means_no_holder() {
        assert!(resolve_holder(&[], NOW, TTL).is_none());
    }

    #[test]
    fn plain_human_comments_are_ignored() {
        let c = vec![(1, "can someone pick this up?".to_string())];
        assert!(resolve_holder(&c, NOW, TTL).is_none());
    }

    #[test]
    fn single_fresh_claim_wins() {
        let c = vec![(5, mk(Action::Claim, "a:1", NOW))];
        let h = resolve_holder(&c, NOW, TTL).unwrap();
        assert_eq!(h.comment_id, 5);
        assert_eq!(h.marker.owner, "a:1");
    }

    #[test]
    fn lowest_comment_id_wins_regardless_of_vec_order() {
        let c = vec![
            (9, mk(Action::Claim, "b:2", NOW)),
            (4, mk(Action::Claim, "a:1", NOW)),
        ];
        assert_eq!(resolve_holder(&c, NOW, TTL).unwrap().marker.owner, "a:1");
    }

    #[test]
    fn stale_claim_is_not_a_holder_but_fresh_one_is() {
        let stale = vec![(1, mk(Action::Claim, "a:1", NOW - TTL - 1))];
        assert!(resolve_holder(&stale, NOW, TTL).is_none());

        let fresh = vec![(1, mk(Action::Claim, "a:1", NOW - TTL + 1))];
        assert!(resolve_holder(&fresh, NOW, TTL).is_some());
    }

    #[test]
    fn a_progress_marker_refreshes_an_otherwise_stale_claim() {
        let c = vec![
            (1, mk(Action::Claim, "a:1", NOW - TTL - 500)),
            (2, mk(Action::Progress, "a:1", NOW - 10)),
        ];
        let h = resolve_holder(&c, NOW, TTL).unwrap();
        assert_eq!(h.comment_id, 1, "holder is still the original claim");
        assert_eq!(h.marker.owner, "a:1");
    }

    #[test]
    fn stale_holder_yields_to_a_later_fresh_claimant() {
        let c = vec![
            (1, mk(Action::Claim, "a:1", NOW - TTL - 1)),
            (2, mk(Action::Claim, "b:2", NOW)),
        ];
        assert_eq!(resolve_holder(&c, NOW, TTL).unwrap().marker.owner, "b:2");
    }

    #[test]
    fn ceding_retires_that_owners_claim_and_promotes_the_next() {
        let c = vec![
            (1, mk(Action::Claim, "a:1", NOW)),
            (2, mk(Action::Claim, "b:2", NOW)),
            (3, mk(Action::Cede, "a:1", NOW)),
        ];
        assert_eq!(resolve_holder(&c, NOW, TTL).unwrap().marker.owner, "b:2");
    }

    #[test]
    fn done_means_no_holder() {
        let c = vec![
            (1, mk(Action::Claim, "a:1", NOW)),
            (2, mk(Action::Done, "a:1", NOW)),
        ];
        assert!(resolve_holder(&c, NOW, TTL).is_none());
    }

    #[test]
    fn cede_then_fresh_reclaim_by_same_owner_wins() {
        let c = vec![
            (1, mk(Action::Claim, "a:1", NOW - 500)),
            (2, mk(Action::Cede, "a:1", NOW - 400)),
            (3, mk(Action::Claim, "a:1", NOW - 10)), // fresh, uncontested reclaim
        ];
        let h = resolve_holder(&c, NOW, TTL).unwrap();
        assert_eq!(h.comment_id, 3, "the later reclaim, not the ceded claim");
        assert_eq!(h.marker.owner, "a:1");
    }

    #[test]
    fn done_from_a_never_claimant_does_not_kill_a_fresher_claim() {
        let c = vec![
            (1, mk(Action::Done, "c:3", NOW - 5000)), // c:3 never claimed anything
            (2, mk(Action::Claim, "b:2", NOW - 10)),
        ];
        let h = resolve_holder(&c, NOW, TTL).unwrap();
        assert_eq!(h.comment_id, 2);
        assert_eq!(h.marker.owner, "b:2");
    }

    #[test]
    fn ttl_boundary_exactly_at_limit_is_still_live() {
        let c = vec![(1, mk(Action::Claim, "a:1", NOW - TTL))];
        let h = resolve_holder(&c, NOW, TTL).unwrap();
        assert_eq!(h.marker.owner, "a:1");
    }

    #[test]
    fn latest_ts_for_owner_with_no_markers_does_not_panic() {
        let parsed: Vec<(u64, Marker)> = vec![(
            1,
            Marker {
                action: Action::Claim,
                owner: "someone-else".to_string(),
                item: "i".to_string(),
                ts: NOW,
                hash: "h".to_string(),
            },
        )];
        assert_eq!(latest_ts_for("nobody", &parsed), None);
    }

    #[test]
    fn i_hold_is_true_only_for_the_winner() {
        let c = vec![
            (1, mk(Action::Claim, "a:1", NOW)),
            (2, mk(Action::Claim, "b:2", NOW)),
        ];
        assert!(i_hold(&c, "a:1", NOW, TTL));
        assert!(!i_hold(&c, "b:2", NOW, TTL), "loser must cede, not proceed");
    }
}
