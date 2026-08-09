//! Read-only queue-depth signal for the bridge's pull queue -- how many
//! labelled issues are open, how many are currently unclaimed, and how
//! stale the oldest unclaimed one is. Queryable from anywhere (no local
//! daemon state needed, only the GitHub API) since the point is to give an
//! agent something to check *before* deciding whether to route work onto
//! the queue (`handoff` `recipient="github"`) or keep it local: an empty or
//! fast-clearing queue suggests capacity exists somewhere; unclaimed issues
//! piling up suggests nothing is currently pulling from it.

use crate::github::bridge::claim as claim_rules;
use crate::github::{Client, GitHubError, RepoId, issues};

#[derive(Debug, Clone, serde::Serialize)]
pub struct QueueStatus {
    pub total_open: usize,
    pub unclaimed: usize,
    /// Seconds since the oldest unclaimed issue WITH A KNOWN `created_at`
    /// was opened. `None` when no unclaimed issue has a known `created_at`
    /// (including when `unclaimed` is 0) -- check
    /// `unclaimed_with_unknown_age` before treating this as "no unclaimed
    /// issues are old": a `None`/low value here can coexist with unclaimed
    /// issues of truly unknown age.
    pub oldest_unclaimed_age_secs: Option<i64>,
    /// Unclaimed issues whose `created_at` was missing or unparseable, and
    /// so could not factor into `oldest_unclaimed_age_secs` at all -- an
    /// explicit signal that the "oldest" figure may be missing an even
    /// older issue, rather than silently treating it as accurate.
    pub unclaimed_with_unknown_age: usize,
    /// Distinct claim owners currently holding at least one issue, with
    /// their held count -- a rough proxy for how many workstations are
    /// actively pulling from this queue right now. Sorted by owner name
    /// for stable output.
    pub claims_by_owner: Vec<(String, usize)>,
}

fn parse_unix(ts: &Option<String>) -> Option<i64> {
    ts.as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
}

pub fn queue_status(
    client: &Client,
    repo: &RepoId,
    queue_label: &str,
    now: i64,
    ttl_secs: i64,
) -> Result<QueueStatus, GitHubError> {
    let open_issues = issues::list_filtered(client, repo, "open", Some(queue_label), None)?;

    // One repo-wide comments listing instead of one `list_comments` call per
    // open issue: a queue of N issues used to cost at least N+1 requests
    // (plus per-issue pagination), which can exhaust the API quota or time
    // out on a large queue.
    let mut comments_by_issue: std::collections::HashMap<u64, Vec<(u64, String)>> =
        Default::default();
    for comment in issues::list_all_comments(client, repo, None)? {
        if let Some(number) = comment.issue_number() {
            comments_by_issue
                .entry(number)
                .or_default()
                .push((comment.id, comment.body));
        }
    }
    let no_comments: Vec<(u64, String)> = Vec::new();

    let mut unclaimed = 0;
    let mut unclaimed_with_unknown_age = 0;
    let mut oldest_unclaimed_created_at: Option<i64> = None;
    let mut claims_by_owner: std::collections::BTreeMap<String, usize> = Default::default();

    for issue in &open_issues {
        let comments = comments_by_issue.get(&issue.number).unwrap_or(&no_comments);
        match claim_rules::resolve_holder(comments, now, ttl_secs) {
            Some(holder) => {
                *claims_by_owner.entry(holder.marker.owner).or_insert(0) += 1;
            }
            None => {
                unclaimed += 1;
                match parse_unix(&issue.created_at) {
                    Some(created_at) => {
                        oldest_unclaimed_created_at = Some(
                            oldest_unclaimed_created_at.map_or(created_at, |c| c.min(created_at)),
                        );
                    }
                    None => unclaimed_with_unknown_age += 1,
                }
            }
        }
    }

    Ok(QueueStatus {
        total_open: open_issues.len(),
        unclaimed,
        oldest_unclaimed_age_secs: oldest_unclaimed_created_at.map(|c| (now - c).max(0)),
        unclaimed_with_unknown_age,
        claims_by_owner: claims_by_owner.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::RepoId;
    use crate::github::test_support::{MockResponse, MockServer};

    fn repo() -> RepoId {
        RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    fn issue_json(number: u64, created_at: &str) -> String {
        format!(
            r#"{{"number":{number},"html_url":"https://x/{number}","state":"open","title":"t{number}","created_at":"{created_at}"}}"#
        )
    }

    fn claim_comment(owner: &str, ts: i64, issue_number: u64) -> String {
        format!(
            r#"{{"id":1,"user":{{"login":"bot"}},"body":"claiming\n\n<!-- agentflare:v1 action=claim owner={owner} item=x ts={ts} hash=h -->","issue_url":"https://api.github.com/repos/o/r/issues/{issue_number}"}}"#
        )
    }

    #[test]
    fn counts_unclaimed_and_tracks_the_oldest() {
        let server = MockServer::start(vec![
            // issue_list
            MockResponse::json(
                200,
                &format!(
                    "[{},{}]",
                    issue_json(1, "2026-01-01T00:00:00Z"),
                    issue_json(2, "2026-01-02T00:00:00Z")
                ),
            ),
            // one repo-wide comments listing, not one call per issue
            MockResponse::json(200, "[]"),
        ]);
        let client = server.client(None);
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-03T00:00:00Z")
            .unwrap()
            .timestamp();

        let status = queue_status(&client, &repo(), "agentflare", now, 300).unwrap();
        assert_eq!(status.total_open, 2);
        assert_eq!(status.unclaimed, 2);
        // Oldest unclaimed is issue 1, created 2026-01-01 -- 2 days before `now`.
        assert_eq!(status.oldest_unclaimed_age_secs, Some(2 * 24 * 60 * 60));
        assert_eq!(status.unclaimed_with_unknown_age, 0);
        assert!(status.claims_by_owner.is_empty());
        assert_eq!(
            server.requests().len(),
            2,
            "must cost exactly one issue-list request plus one repo-wide comments \
             request, regardless of how many issues are open"
        );
    }

    #[test]
    fn counts_claimed_issues_by_owner() {
        let now = 1_000_000i64;
        let server = MockServer::start(vec![
            MockResponse::json(200, &format!("[{}]", issue_json(1, "2026-01-01T00:00:00Z"))),
            MockResponse::json(
                200,
                &format!("[{}]", claim_comment("workstation-a", now - 10, 1)),
            ),
        ]);
        let client = server.client(None);

        let status = queue_status(&client, &repo(), "agentflare", now, 300).unwrap();
        assert_eq!(status.total_open, 1);
        assert_eq!(status.unclaimed, 0);
        assert_eq!(status.oldest_unclaimed_age_secs, None);
        assert_eq!(status.unclaimed_with_unknown_age, 0);
        assert_eq!(
            status.claims_by_owner,
            vec![("workstation-a".to_string(), 1)]
        );
    }

    #[test]
    fn an_expired_claim_counts_as_unclaimed() {
        let now = 1_000_000i64;
        let server = MockServer::start(vec![
            MockResponse::json(200, &format!("[{}]", issue_json(1, "2026-01-01T00:00:00Z"))),
            // Claim marker is way older than the ttl -- stale, must not count as held.
            MockResponse::json(
                200,
                &format!("[{}]", claim_comment("workstation-a", now - 10_000, 1)),
            ),
        ]);
        let client = server.client(None);

        let status = queue_status(&client, &repo(), "agentflare", now, 300).unwrap();
        assert_eq!(status.unclaimed, 1);
        assert!(status.claims_by_owner.is_empty());
    }

    #[test]
    fn a_comment_on_a_different_issue_does_not_claim_this_one() {
        // Regression guard for the batched-comments rewrite: comments must be
        // grouped by `issue_url`, not applied to every issue in the queue.
        let now = 1_000_000i64;
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                &format!(
                    "[{},{}]",
                    issue_json(1, "2026-01-01T00:00:00Z"),
                    issue_json(2, "2026-01-01T00:00:00Z")
                ),
            ),
            // The only claim comment belongs to issue 2 -- issue 1 must stay
            // unclaimed.
            MockResponse::json(
                200,
                &format!("[{}]", claim_comment("workstation-a", now - 10, 2)),
            ),
        ]);
        let client = server.client(None);

        let status = queue_status(&client, &repo(), "agentflare", now, 300).unwrap();
        assert_eq!(status.unclaimed, 1);
        assert_eq!(
            status.claims_by_owner,
            vec![("workstation-a".to_string(), 1)]
        );
    }

    #[test]
    fn an_unclaimed_issue_with_no_created_at_is_reported_as_unknown_age_not_ignored() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                r#"[{"number":1,"html_url":"u","state":"open","title":"t"}]"#,
            ),
            MockResponse::json(200, "[]"),
        ]);
        let client = server.client(None);
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-03T00:00:00Z")
            .unwrap()
            .timestamp();

        let status = queue_status(&client, &repo(), "agentflare", now, 300).unwrap();
        assert_eq!(status.unclaimed, 1);
        assert_eq!(
            status.oldest_unclaimed_age_secs, None,
            "no unclaimed issue has a known created_at"
        );
        assert_eq!(
            status.unclaimed_with_unknown_age, 1,
            "the missing timestamp must be counted, not silently dropped"
        );
    }
}
