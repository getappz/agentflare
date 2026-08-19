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
