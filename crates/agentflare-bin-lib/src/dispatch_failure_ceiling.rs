//! Consecutive identical work-failure counting for the daemon's auto-redispatch
//! ceiling (item #506). Comments are the source of truth — same pattern as
//! `supervisor::CI_SELF_REPAIR_MARKER` / `quota::decide::SELF_REPAIR_CAP`,
//! both of which live in the root `agentflare` binary crate, not here: this
//! crate has no dependency on it, so nothing below enforces that these
//! marker strings stay in sync with the formatters that emit them — treat
//! any change to a marker's text as a cross-crate rename.

/// Prefix on every failure comment from `cli::work::release_and_comment`
/// (root crate) — keep in sync with that formatter by hand.
pub const WORK_FAILURE_MARKER: &str = "## agentflare work — failed";
/// Prefix on the comment `mcp_server::item` (root crate) posts when a real
/// commit's push/PR creation failed — keep in sync with that formatter by
/// hand. Terminal for dead-claim auto-release (item #649), deliberately NOT
/// part of `failure_reason`'s dispatch-ceiling counting.
pub const PR_CREATION_FAILED_MARKER: &str = "## agentflare work — PR creation failed";
/// Prefix on the comment `mcp_server::item` (root crate) posts when
/// auto-commit itself failed — same sync/terminal notes as above.
pub const COMMIT_FAILED_MARKER: &str = "## agentflare work — commit failed";

/// Whether a comment body is any terminal work-failure marker: the generic
/// `WORK_FAILURE_MARKER` plus the two hard-failure paths in
/// `mcp_server::item` that never go through `release_and_comment`.
/// Used by dead-claim auto-release / force-takeover evidence, NOT by the
/// dispatch-ceiling counters (those key on `failure_reason` only).
pub fn is_terminal_work_failure(body: &str) -> bool {
    [
        WORK_FAILURE_MARKER,
        PR_CREATION_FAILED_MARKER,
        COMMIT_FAILED_MARKER,
    ]
    .iter()
    .any(|m| body.starts_with(m))
}
/// A successful run breaks a consecutive-identical-failure streak.
pub const WORK_SUCCESS_MARKER: &str = "## agentflare work — complete";
/// Prefix on a discovery-tick dispatch comment (see `dispatch_item` in
/// `supervisor.rs`, root crate) — one marker per dispatch cycle; intra-job
/// retries do not post another.
pub const DISPATCH_MARKER: &str = "## supervisor — dispatched";
/// Prefix on the supervisor comment posted when the ceiling trips.
pub const DISPATCH_FAILURE_CAP_MARKER: &str = "## supervisor — identical failure cap reached";

/// Prefix on the comment `cli::work` (root crate) posts when it moved an
/// item to another agent because the one running it ran out of
/// credit/quota or hit a long rate limit. Neutral for both caps: the agent
/// failed, not the item.
pub const AGENT_FAILOVER_MARKER: &str = "## agentflare work — moved to another agent";
/// Prefix on the comment posted when the agent ran out and no other agent was
/// available, so the run waits for the agent's reset. Neutral, same reason.
pub const AGENT_UNAVAILABLE_MARKER: &str = "## agentflare work — agent unavailable";
/// Prefix on the comment posted when a run was stopped on request (workflow
/// cancelled, or paused). Neutral for both caps, and tells the terminal-job
/// hook not to put the item back on `ready-for-work` (see
/// [`stopped_on_request`]).
pub const STOPPED_ON_REQUEST_MARKER: &str = "## agentflare work — stopped on request";

/// Outcome comments that say nothing about whether the item itself is
/// broken -- a cycle whose latest outcome is one of these is skipped by both
/// counts: it neither adds to a streak nor breaks one.
fn is_neutral_outcome(body: &str) -> bool {
    [
        AGENT_FAILOVER_MARKER,
        AGENT_UNAVAILABLE_MARKER,
        STOPPED_ON_REQUEST_MARKER,
    ]
    .iter()
    .any(|m| body.starts_with(m))
}

/// A segment's latest outcome comment is a neutral one (a later real failure
/// on a retry of the same job still counts).
fn segment_ended_neutral(segment: &[agentflare_backend::comment::ItemComment]) -> bool {
    segment
        .iter()
        .rev()
        .find(|c| failure_reason(&c.body).is_some() || is_neutral_outcome(&c.body))
        .is_some_and(|c| is_neutral_outcome(&c.body))
}

/// Whether the item's most recent outcome is a stop-on-request (cancel or
/// pause) -- the terminal-job hook must then leave it off `ready-for-work`.
pub fn stopped_on_request(comments: &[agentflare_backend::comment::ItemComment]) -> bool {
    comments
        .iter()
        .rev()
        .find(|c| {
            [
                WORK_FAILURE_MARKER,
                WORK_SUCCESS_MARKER,
                DISPATCH_MARKER,
                AGENT_FAILOVER_MARKER,
                AGENT_UNAVAILABLE_MARKER,
                STOPPED_ON_REQUEST_MARKER,
            ]
            .iter()
            .any(|m| c.body.starts_with(m))
        })
        .is_some_and(|c| c.body.starts_with(STOPPED_ON_REQUEST_MARKER))
}

/// After this many consecutive dispatch cycles whose terminal failure reason
/// is identical/near-identical, the daemon stops swapping an item back to
/// `ready-for-work` for auto-redispatch.
pub const DISPATCH_FAILURE_CAP: u32 = 3;

/// Looser counterpart to `DISPATCH_FAILURE_CAP`: after this many consecutive
/// dispatch cycles that each ended WITHOUT a clean success — regardless of
/// whether the failure reason matches the others, or a cycle recorded no
/// failure reason at all (job orphaned/killed before posting
/// `WORK_FAILURE_MARKER`) — the daemon stops auto-redispatching. Closes the
/// gap `DISPATCH_FAILURE_CAP` alone leaves open: a mix of genuinely
/// different failure classes (worktree lock contention, step-dependency
/// failures, an expired CLI auth session producing zero stdout, a daemon
/// restart orphaning the job mid-run) each reset the identical-reason
/// streak before it ever reached `DISPATCH_FAILURE_CAP`, so an item could
/// accumulate hundreds of dispatch cycles with no cap ever tripping (item
/// #164 hit 400+, only 2 of which ever landed on the identical-reason cap).
/// Deliberately looser than `DISPATCH_FAILURE_CAP` — a single daemon death
/// mid-job still isn't deterministic evidence of a real bug on its own, so
/// this gives more room before giving up than the identical-reason cap does.
pub const DISPATCH_FAILURE_CAP_ANY_REASON: u32 = 6;

pub fn failure_reason(body: &str) -> Option<&str> {
    let rest = body.strip_prefix(WORK_FAILURE_MARKER)?;
    rest.strip_prefix("\n\n").or(Some(""))
}

/// Near-identical: collapse whitespace so formatting-only diffs still match.
pub fn normalize_failure_reason(reason: &str) -> String {
    reason.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One entry per dispatch cycle: the normalized terminal failure reason for
/// the segment after each `DISPATCH_MARKER` comment (through the next dispatch
/// marker or end of thread), plus whether that segment is where the cap was
/// last reported. Intra-job retries only update that segment's reason — they
/// do not add cycles.
struct DispatchCycle {
    reason: String,
    /// This cycle's segment contains `DISPATCH_FAILURE_CAP_MARKER` — the cap
    /// was already reported for it. A later streak must not chain across it
    /// even if the reason repeats, or a post-redispatch retry would re-trip
    /// the cap with no retry budget.
    cap_already_reported: bool,
}

fn dispatch_cycle_failure_reasons(
    comments: &[agentflare_backend::comment::ItemComment],
) -> Vec<DispatchCycle> {
    let dispatch_indices: Vec<usize> = comments
        .iter()
        .enumerate()
        .filter(|(_, c)| c.body.starts_with(DISPATCH_MARKER))
        .map(|(i, _)| i)
        .collect();

    let mut cycles = Vec::new();
    for (idx, &start) in dispatch_indices.iter().enumerate() {
        let end = dispatch_indices
            .get(idx + 1)
            .copied()
            .unwrap_or(comments.len());
        let segment = &comments[start..end];
        if segment
            .iter()
            .any(|c| c.body.starts_with(WORK_SUCCESS_MARKER))
        {
            cycles.clear();
            continue;
        }
        if segment_ended_neutral(segment) {
            continue;
        }
        let Some(reason) = segment
            .iter()
            .rev()
            .find_map(|c| failure_reason(&c.body).map(normalize_failure_reason))
        else {
            // No terminal failure recorded for this cycle (e.g. an
            // orphan-restart via `restore_ready_for_work`, which
            // deliberately posts no marker — a daemon death mid-job is not
            // evidence of a deterministic failure class). Its outcome is
            // unknown, so it must not silently bridge an identical reason
            // across it as if the cycles were adjacent.
            cycles.clear();
            continue;
        };
        let cap_already_reported = segment
            .iter()
            .any(|c| c.body.starts_with(DISPATCH_FAILURE_CAP_MARKER));
        cycles.push(DispatchCycle {
            reason,
            cap_already_reported,
        });
    }
    cycles
}

/// One dispatch cycle's coarse outcome: a clean success, or anything else
/// (a recorded failure with any reason, or no recorded outcome at all — an
/// orphaned/killed job that never reached `release_and_comment`). Unlike
/// `DispatchCycle`, this deliberately drops *which* reason a failure had —
/// `consecutive_failure_count_any_reason` below counts every non-success
/// cycle, so the specific reason (or its absence) doesn't matter to it.
enum CoarseOutcome {
    Success,
    NotSuccess { cap_already_reported: bool },
}

fn dispatch_cycle_coarse_outcomes(
    comments: &[agentflare_backend::comment::ItemComment],
) -> Vec<CoarseOutcome> {
    let dispatch_indices: Vec<usize> = comments
        .iter()
        .enumerate()
        .filter(|(_, c)| c.body.starts_with(DISPATCH_MARKER))
        .map(|(i, _)| i)
        .collect();

    dispatch_indices
        .iter()
        .enumerate()
        .filter_map(|(idx, &start)| {
            let end = dispatch_indices
                .get(idx + 1)
                .copied()
                .unwrap_or(comments.len());
            let segment = &comments[start..end];
            if segment
                .iter()
                .any(|c| c.body.starts_with(WORK_SUCCESS_MARKER))
            {
                Some(CoarseOutcome::Success)
            } else if segment_ended_neutral(segment) {
                None
            } else {
                let cap_already_reported = segment
                    .iter()
                    .any(|c| c.body.starts_with(DISPATCH_FAILURE_CAP_MARKER));
                Some(CoarseOutcome::NotSuccess {
                    cap_already_reported,
                })
            }
        })
        .collect()
}

/// Consecutive non-success dispatch cycles, counting back from the newest,
/// regardless of whether each cycle's failure reason matches the others or
/// was even recorded. The coarser counterpart to
/// `consecutive_identical_failure_count`: that function resets its streak on
/// any reason change *or* an unrecorded outcome (by design, so it never
/// falsely bridges two identical-looking failures across an unknown cycle)
/// — which means a mix of different transient failure classes can starve it
/// indefinitely. This one only stops at an actual success or a cycle that
/// already reported the cap, so it still bounds the total no matter how
/// varied (or unrecorded) the failures are.
pub fn consecutive_failure_count_any_reason(
    comments: &[agentflare_backend::comment::ItemComment],
) -> u32 {
    let mut count = 0u32;
    for outcome in dispatch_cycle_coarse_outcomes(comments).iter().rev() {
        match outcome {
            CoarseOutcome::Success => break,
            CoarseOutcome::NotSuccess {
                cap_already_reported: true,
            } => break,
            CoarseOutcome::NotSuccess {
                cap_already_reported: false,
            } => count += 1,
        }
    }
    count
}

/// Walks dispatch-cycle terminal failure reasons (oldest-first) from newest
/// backward, counting consecutive cycles whose normalized reason matches the
/// latest one. Stops at the first older cycle with a different reason, at a
/// success comment, an unrecorded-outcome cycle (both clear the streak when
/// building the cycle list), or a cycle that already reported the cap (so a
/// post-redispatch retry gets a fresh budget instead of re-tripping
/// immediately).
pub fn consecutive_identical_failure_count(
    comments: &[agentflare_backend::comment::ItemComment],
) -> u32 {
    let cycles = dispatch_cycle_failure_reasons(comments);
    let mut iter = cycles.iter().rev();
    let Some(newest) = iter.next() else {
        return 0;
    };
    // Defensive: if the newest cycle's own segment already reported the cap
    // (should not happen in practice — the cap comment is only posted after
    // this count decides `at_cap`), stop right there rather than scanning
    // into an already-resolved streak.
    if newest.cap_already_reported {
        return 1;
    }
    let mut count = 1u32;
    for cycle in iter {
        // A cycle that already reported the cap is a hard boundary — even a
        // matching reason must not chain a post-redispatch retry onto an
        // already-tripped streak, or it would re-trip with no retry budget.
        if cycle.cap_already_reported || cycle.reason != newest.reason {
            break;
        }
        count += 1;
    }
    count
}

pub fn latest_failure_reason(
    comments: &[agentflare_backend::comment::ItemComment],
) -> Option<String> {
    comments
        .iter()
        .rev()
        .find_map(|c| failure_reason(&c.body).map(normalize_failure_reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentflare_backend::comment::ItemComment;

    fn comment(body: &str) -> ItemComment {
        ItemComment {
            id: "c1".into(),
            item_id: "item-1".into(),
            author_agent: "test".into(),
            body: body.into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn counts_consecutive_identical_dispatch_cycles() {
        let err = "judge reply was not valid JSON";
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: c")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
        ];
        assert_eq!(consecutive_identical_failure_count(&comments), 3);
    }

    #[test]
    fn intra_job_retries_do_not_inflate_the_dispatch_cycle_count() {
        let err = "judge reply was not valid JSON";
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
        ];
        assert_eq!(consecutive_identical_failure_count(&comments), 1);
    }

    #[test]
    fn different_reason_resets_the_streak() {
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror A")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror B")),
        ];
        assert_eq!(consecutive_identical_failure_count(&comments), 1);
    }

    #[test]
    fn success_breaks_the_streak() {
        let err = "same error";
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            comment(&format!("{WORK_SUCCESS_MARKER}\n\nok")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
        ];
        assert_eq!(consecutive_identical_failure_count(&comments), 1);
    }

    #[test]
    fn cap_comment_gives_a_fresh_streak_budget_after_manual_redispatch() {
        let err = "judge reply was not valid JSON";
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: c")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            comment(&format!(
                "{DISPATCH_FAILURE_CAP_MARKER}\n\n3 consecutive..."
            )),
            // Human fixes the root cause and runs `item action=redispatch`;
            // the daemon dispatches a new cycle that happens to fail with
            // the same normalized reason.
            comment(&format!("{DISPATCH_MARKER}\n\njob: d")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
        ];
        assert_eq!(
            consecutive_identical_failure_count(&comments),
            1,
            "a cycle after the cap was reported must not chain onto the \
             already-tripped streak, or the operator gets zero retry budget"
        );
    }

    #[test]
    fn unrecorded_outcome_cycle_breaks_adjacency() {
        let err = "judge reply was not valid JSON";
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            // Daemon restart mid-job: `restore_ready_for_work` deliberately
            // posts no marker for this cycle.
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: c")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
        ];
        assert_eq!(
            consecutive_identical_failure_count(&comments),
            1,
            "an unrecorded-outcome cycle must not silently bridge two \
             identical-reason cycles into a false consecutive streak"
        );
    }

    #[test]
    fn any_reason_count_accumulates_across_different_reasons() {
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror A")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror B")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: c")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror C")),
        ];
        assert_eq!(
            consecutive_failure_count_any_reason(&comments),
            3,
            "different reasons must still accumulate toward the coarser cap"
        );
    }

    #[test]
    fn any_reason_count_includes_unrecorded_outcome_cycles() {
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror A")),
            // Orphan-restart: dispatched again with no failure marker at
            // all (`restore_ready_for_work` posts none).
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: c")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror C")),
        ];
        assert_eq!(
            consecutive_failure_count_any_reason(&comments),
            3,
            "an unrecorded-outcome cycle must still count toward the coarser \
             cap, unlike the identical-reason streak"
        );
    }

    #[test]
    fn any_reason_count_resets_on_success() {
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror A")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{WORK_SUCCESS_MARKER}\n\nok")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: c")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror C")),
        ];
        assert_eq!(consecutive_failure_count_any_reason(&comments), 1);
    }

    #[test]
    fn any_reason_count_stops_at_already_reported_cap() {
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror A")),
            comment(&format!("{DISPATCH_FAILURE_CAP_MARKER}\n\n... cap reached")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nerror B")),
        ];
        assert_eq!(
            consecutive_failure_count_any_reason(&comments),
            1,
            "a cycle after the cap was reported must not chain onto the \
             already-tripped streak, same rationale as the identical-reason cap"
        );
    }

    #[test]
    fn any_reason_count_zero_with_no_dispatch_cycles() {
        assert_eq!(consecutive_failure_count_any_reason(&[]), 0);
    }

    #[test]
    fn failover_and_unavailable_cycles_do_not_count_toward_either_cap() {
        let err = "judge reply was not valid JSON";
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!(
                "{AGENT_FAILOVER_MARKER}\n\nmoved from claude-code to codex: out of credit"
            )),
            comment(&format!("{DISPATCH_MARKER}\n\njob: c")),
            comment(&format!(
                "{AGENT_UNAVAILABLE_MARKER}\n\ncodex: usage limit reached"
            )),
            comment(&format!("{DISPATCH_MARKER}\n\njob: d")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\n{err}")),
        ];
        assert_eq!(
            consecutive_identical_failure_count(&comments),
            2,
            "neutral cycles neither count nor break the identical streak"
        );
        assert_eq!(consecutive_failure_count_any_reason(&comments), 2);
        let only_neutral = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{AGENT_FAILOVER_MARKER}\n\nmoved")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{AGENT_UNAVAILABLE_MARKER}\n\nwaiting")),
        ];
        assert_eq!(consecutive_identical_failure_count(&only_neutral), 0);
        assert_eq!(consecutive_failure_count_any_reason(&only_neutral), 0);
    }

    #[test]
    fn a_real_failure_on_a_later_retry_of_the_same_job_still_counts() {
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{AGENT_UNAVAILABLE_MARKER}\n\nwaiting")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nreal bug")),
        ];
        assert_eq!(consecutive_failure_count_any_reason(&comments), 1);
    }

    #[test]
    fn stopped_on_request_reads_the_latest_outcome() {
        let stopped = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!("{STOPPED_ON_REQUEST_MARKER}\n\ncancelled")),
        ];
        assert!(stopped_on_request(&stopped));
        assert_eq!(consecutive_failure_count_any_reason(&stopped), 0);
        let redispatched = vec![
            comment(&format!("{STOPPED_ON_REQUEST_MARKER}\n\ncancelled")),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!("{WORK_FAILURE_MARKER}\n\nboom")),
        ];
        assert!(!stopped_on_request(&redispatched));
    }

    #[test]
    fn whitespace_normalization_treats_near_identical_as_same() {
        let comments = vec![
            comment(&format!("{DISPATCH_MARKER}\n\njob: a")),
            comment(&format!(
                "{WORK_FAILURE_MARKER}\n\njudge reply was not   valid JSON"
            )),
            comment(&format!("{DISPATCH_MARKER}\n\njob: b")),
            comment(&format!(
                "{WORK_FAILURE_MARKER}\n\njudge reply was not valid JSON"
            )),
        ];
        assert_eq!(consecutive_identical_failure_count(&comments), 2);
    }
}
