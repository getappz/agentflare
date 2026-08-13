//! Builds and runs the per-work-item `flare-workflow` pipeline: `coder` →
//! a bounded `review_or_fix` loop → `finalize`. See
//! `docs` item #110 for the design; corrected against the crate's real
//! Rust builder API (not the JSON/OpenFang schema — `finalize` runs real
//! Rust logic, not an agent prompt).

/// Cap on review/fix cycles before an item is gated for a human instead of
/// looping forever on an agent that can't converge. Mirrors
/// `quota::decide::SELF_REPAIR_CAP`'s existing cap-constant pattern.
pub(crate) const MAX_REVIEW_CYCLES: u32 = 3;

/// `flare_workflow::WorkflowId` name for this pipeline definition —
/// registered once at daemon boot (see `src/dashboard/server.rs`) and
/// referenced by every dispatched item's run.
pub(crate) const WORKFLOW_ID: &str = "agentflare-work-item";

/// Per-run state threaded through `coder` → `review_or_fix` → `finalize`.
/// `flare_workflow::WorkflowContext::data` persists and mutates across
/// steps within a run — this is where step results live, not the
/// `input`/`output` string channel (which only carries the loop's own
/// phase signal, see `build_review_or_fix_step`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkItemData {
    pub reply_text: String,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    /// Set when `coder` detects an `AGENTFLARE_HOLD:` signal — short-circuits
    /// the rest of the pipeline straight to `item_release` (see Task 4).
    pub hold_reason: Option<String>,
    /// Latest unresolved reviewer findings, if any — read by `finalize`'s
    /// cap-exceeded path to post a useful gate comment.
    pub review_issues: Option<String>,
    pub pr_url: Option<String>,
}

impl flare_workflow::WorkflowData for WorkItemData {
    fn workflow_type() -> &'static str {
        WORKFLOW_ID
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_item_data_round_trips_through_json() {
        let data = WorkItemData {
            reply_text: "did the thing".into(),
            session_id: Some("sess-1".into()),
            cost_usd: Some(0.42),
            hold_reason: None,
            review_issues: Some("- fix the thing".into()),
            pr_url: Some("https://github.com/x/y/pull/1".into()),
        };
        let json = serde_json::to_string(&data).unwrap();
        let back: WorkItemData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reply_text, "did the thing");
        assert_eq!(back.pr_url.as_deref(), Some("https://github.com/x/y/pull/1"));
    }
}
