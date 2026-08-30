//! Consecutive identical work-failure counting for the daemon's auto-redispatch
//! ceiling (item #506). Comments are the source of truth — same pattern as
//! `supervisor::CI_SELF_REPAIR_MARKER` / `quota::decide::SELF_REPAIR_CAP`.

/// Prefix on every failure comment from `cli::work::release_and_comment` —
/// keep in sync with that formatter.
pub(crate) const WORK_FAILURE_MARKER: &str = "## agentflare work — failed";
/// A successful run breaks a consecutive-identical-failure streak.
pub(crate) const WORK_SUCCESS_MARKER: &str = "## agentflare work — complete";
/// Prefix on a discovery-tick dispatch comment (see `dispatch_item` in
/// `supervisor.rs`) — one marker per dispatch cycle; intra-job retries do
/// not post another.
pub(crate) const DISPATCH_MARKER: &str = "## supervisor — dispatched";
/// Prefix on the supervisor comment posted when the ceiling trips.
pub(crate) const DISPATCH_FAILURE_CAP_MARKER: &str =
    "## supervisor — identical failure cap reached";

/// After this many consecutive dispatch cycles whose terminal failure reason
/// is identical/near-identical, the daemon stops swapping an item back to
/// `ready-for-work` for auto-redispatch.
pub(crate) const DISPATCH_FAILURE_CAP: u32 = 3;

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
pub(crate) const DISPATCH_FAILURE_CAP_ANY_REASON: u32 = 6;

pub(crate) fn failure_reason(body: &str) -> Option<&str> {
    let rest = body.strip_prefix(WORK_FAILURE_MARKER)?;
    rest.strip_prefix("\n\n").or(Some(""))
}

/// Near-identical: collapse whitespace so formatting-only diffs still match.
pub(crate) fn normalize_failure_reason(reason: &str) -> String {
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
        .map(|(idx, &start)| {
            let end = dispatch_indices
                .get(idx + 1)
                .copied()
                .unwrap_or(comments.len());
            let segment = &comments[start..end];
            if segment
                .iter()
                .any(|c| c.body.starts_with(WORK_SUCCESS_MARKER))
            {
                CoarseOutcome::Success
            } else {
                let cap_already_reported = segment
                    .iter()
                    .any(|c| c.body.starts_with(DISPATCH_FAILURE_CAP_MARKER));
                CoarseOutcome::NotSuccess {
                    cap_already_reported,
                }
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
pub(crate) fn consecutive_failure_count_any_reason(
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
pub(crate) fn consecutive_identical_failure_count(
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

pub(crate) fn latest_failure_reason(
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
