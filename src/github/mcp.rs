//! Bridges the GitHub operations to the `flare_git` MCP tool. Kept thin: the
//! `mcp_server` tool method validates fields and calls into here; this module
//! classifies `GitHubError` so the server can map it to the right `ErrorData`.

use crate::github::GitHubError;
use crate::github::models::{CheckRun, Comment, PullRequest, Review, ReviewComment};

/// `true` = a client mistake / auth problem (invalid_params); `false` =
/// internal/transport failure (internal_error).
pub fn is_client_error(err: &GitHubError) -> bool {
    matches!(
        err,
        GitHubError::NoAuth(_) | GitHubError::Forbidden(_) | GitHubError::NotFound
    )
}

/// Assembles the `pr_status` payload: everything a review-and-fix loop needs
/// to decide its next move, in one call instead of separate `pr_get` /
/// `run_list` / review-comment / issue-comment round trips — and trimmed to
/// just that: no timestamps, no passing checks, no resolved comment threads,
/// no superseded review verdicts. Ask for a bigger window with `since`
/// (passed down to the REST comment fetches) instead of re-reading history
/// the caller already has.
///
/// - `checks`: only non-passing ones; `checks_ok` carries the passing count.
/// - `reviews`: one entry per reviewer — the latest verdict only.
/// - `unresolved`: review comments whose thread GraphQL reports as *not*
///   resolved (`resolved_ids` — see `pulls::resolved_review_comment_ids`).
pub fn pr_status_json(
    pr: &PullRequest,
    checks: &[CheckRun],
    reviews: &[Review],
    review_comments: &[ReviewComment],
    resolved_ids: &std::collections::HashSet<u64>,
    comments: &[Comment],
) -> String {
    let checks_ok = checks
        .iter()
        .filter(|c| c.conclusion.as_deref() == Some("success"))
        .count();
    let checks_open: Vec<_> = checks
        .iter()
        .filter(|c| c.conclusion.as_deref() != Some("success"))
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "state": c.conclusion.clone().unwrap_or_else(|| c.status.clone()),
            })
        })
        .collect();

    // GitHub returns reviews oldest-first, so overwriting by login keeps the
    // most recent verdict per reviewer.
    let mut latest_reviews: std::collections::BTreeMap<&str, &Review> =
        std::collections::BTreeMap::new();
    for r in reviews {
        latest_reviews.insert(r.user.login.as_str(), r);
    }
    let reviews_out: Vec<_> = latest_reviews
        .values()
        .map(|r| {
            let mut o = serde_json::json!({ "by": r.user.login, "state": r.state });
            if !r.body.trim().is_empty() {
                o["body"] = serde_json::Value::String(r.body.clone());
            }
            o
        })
        .collect();

    let unresolved: Vec<_> = review_comments
        .iter()
        .filter(|c| !resolved_ids.contains(&c.id))
        .map(|c| {
            serde_json::json!({
                "by": c.user.login, "path": c.path, "line": c.line, "body": c.body,
            })
        })
        .collect();

    let comments_out: Vec<_> = comments
        .iter()
        .map(|c| serde_json::json!({ "by": c.user.login, "body": c.body }))
        .collect();

    let value = serde_json::json!({
        "n": pr.number,
        "title": pr.title,
        "state": pr.state,
        "draft": pr.draft,
        "mergeable": pr.mergeable_state,
        "adds": pr.additions,
        "dels": pr.deletions,
        "files": pr.changed_files,
        "head": pr.head.as_ref().map(|h| &h.git_ref),
        "base": pr.base.as_ref().map(|h| &h.git_ref),
        "url": pr.html_url,
        "checks_ok": checks_ok,
        "checks": checks_open,
        "reviews": reviews_out,
        "unresolved": unresolved,
        "comments": comments_out,
    });
    serde_json::to_string(&value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_and_notfound_are_client_errors_transport_is_not() {
        assert!(is_client_error(&GitHubError::NoAuth("x".into())));
        assert!(is_client_error(&GitHubError::NotFound));
        assert!(!is_client_error(&GitHubError::RateLimited("x".into())));
        assert!(!is_client_error(&GitHubError::Transport("x".into())));
        assert!(!is_client_error(&GitHubError::Parse("x".into())));
    }

    fn sample_pr() -> PullRequest {
        serde_json::from_value(serde_json::json!({
            "number": 42, "html_url": "https://gh/o/r/pull/42", "state": "open",
            "title": "fix: thing", "draft": false, "mergeable": true,
            "mergeable_state": "clean", "additions": 5, "deletions": 1, "changed_files": 2
        }))
        .unwrap()
    }

    #[test]
    fn pr_status_json_drops_passing_checks_but_counts_them() {
        let pr = sample_pr();
        let checks: Vec<CheckRun> = serde_json::from_value(serde_json::json!([
            {"name": "ci", "status": "completed", "conclusion": "success"},
            {"name": "lint", "status": "completed", "conclusion": "failure"},
            {"name": "deploy", "status": "in_progress", "conclusion": null}
        ]))
        .unwrap();

        let out = pr_status_json(&pr, &checks, &[], &[], &Default::default(), &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["checks_ok"], 1);
        assert_eq!(v["checks"].as_array().unwrap().len(), 2);
        assert_eq!(v["checks"][0]["name"], "lint");
        assert_eq!(v["checks"][0]["state"], "failure");
        assert_eq!(v["checks"][1]["state"], "in_progress");
    }

    #[test]
    fn pr_status_json_keeps_only_the_latest_review_per_author() {
        let pr = sample_pr();
        let reviews: Vec<Review> = serde_json::from_value(serde_json::json!([
            {"user": {"login": "alice"}, "state": "CHANGES_REQUESTED", "body": "fix x"},
            {"user": {"login": "alice"}, "state": "APPROVED", "body": ""}
        ]))
        .unwrap();

        let out = pr_status_json(&pr, &[], &reviews, &[], &Default::default(), &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let out_reviews = v["reviews"].as_array().unwrap();
        assert_eq!(out_reviews.len(), 1);
        assert_eq!(out_reviews[0]["state"], "APPROVED");
        assert!(out_reviews[0].get("body").is_none());
    }

    #[test]
    fn pr_status_json_filters_out_resolved_review_comments() {
        let pr = sample_pr();
        let review_comments: Vec<ReviewComment> = serde_json::from_value(serde_json::json!([
            {"id": 1, "user": {"login": "bob"}, "path": "src/x.rs", "line": 10, "body": "fixed already"},
            {"id": 2, "user": {"login": "bob"}, "path": "src/y.rs", "line": 5, "body": "still open"}
        ]))
        .unwrap();
        let resolved: std::collections::HashSet<u64> = [1].into_iter().collect();

        let out = pr_status_json(&pr, &[], &[], &review_comments, &resolved, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let unresolved = v["unresolved"].as_array().unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0]["body"], "still open");
    }

    #[test]
    fn pr_status_json_has_no_timestamps_or_html_url_noise() {
        let pr = sample_pr();
        let out = pr_status_json(&pr, &[], &[], &[], &Default::default(), &[]);
        assert!(!out.contains("submitted_at"));
        assert!(!out.contains("created_at"));
        assert!(!out.contains("html_url"));
    }
}
