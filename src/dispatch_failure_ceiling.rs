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
/// marker or end of thread). Intra-job retries only update that segment's
/// reason — they do not add cycles.
fn dispatch_cycle_failure_reasons(
    comments: &[agentflare_backend::comment::ItemComment],
) -> Vec<String> {
    let dispatch_indices: Vec<usize> = comments
        .iter()
        .enumerate()
        .filter(|(_, c)| c.body.starts_with(DISPATCH_MARKER))
        .map(|(i, _)| i)
        .collect();

    let mut reasons = Vec::new();
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
            reasons.clear();
            continue;
        }
        let Some(reason) = segment
            .iter()
            .rev()
            .find_map(|c| failure_reason(&c.body).map(normalize_failure_reason))
        else {
            continue;
        };
        reasons.push(reason);
    }
    reasons
}

/// Walks dispatch-cycle terminal failure reasons (oldest-first) from newest
/// backward, counting consecutive cycles whose normalized reason matches the
/// latest one. Stops at the first older cycle with a different reason or at
/// a success comment (which clears the streak when building the cycle list).
pub(crate) fn consecutive_identical_failure_count(
    comments: &[agentflare_backend::comment::ItemComment],
) -> u32 {
    let cycles = dispatch_cycle_failure_reasons(comments);
    let mut signature: Option<&String> = None;
    let mut count = 0u32;
    for reason in cycles.iter().rev() {
        match &signature {
            None => {
                signature = Some(reason);
                count = 1;
            }
            Some(sig) if *sig == reason => count += 1,
            Some(_) => break,
        }
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
