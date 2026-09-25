use crate::github::models::{PullRequest, Review, ReviewComment};
use crate::github::{Client, GitHubError, RepoId};

/// Extractor for the Search API's `{"items": [...], "total_count": N}`
/// envelope, mirroring `actions::workflow_runs`/`check_runs`.
fn search_items(page: &serde_json::Value) -> Vec<serde_json::Value> {
    page.get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

fn create_body(title: &str, head: &str, base: &str, body: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({ "title": title, "head": head, "base": base });
    if let Some(b) = body {
        v["body"] = serde_json::Value::String(b.to_string());
    }
    v
}

pub fn create(
    client: &Client,
    repo: &RepoId,
    title: &str,
    head: &str,
    base: &str,
    body: Option<&str>,
) -> Result<PullRequest, GitHubError> {
    let path = format!("/repos/{}/{}/pulls", repo.owner, repo.repo);
    let json = client.request("POST", &path, Some(create_body(title, head, base, body)))?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn list(client: &Client, repo: &RepoId, state: &str) -> Result<Vec<PullRequest>, GitHubError> {
    let path = format!(
        "/repos/{}/{}/pulls?state={}",
        repo.owner,
        repo.repo,
        crate::github::encode_query(state)
    );
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

/// Finds an existing PR -- open, merged, or manually closed -- whose
/// `head.ref` matches `branch`. Callers that auto-open a PR for a branch
/// should check this first: GitHub's own API only rejects a duplicate while
/// the existing PR is still open, but once it's merged, a second PR against
/// the same branch is perfectly legal to create, which is how `item done`
/// re-running on an already-merged branch ended up opening a redundant PR
/// (2026-07-25, PR #328 duplicating already-merged #327).
///
/// Asks GitHub for just this branch (`head=<owner>:<branch>`) in a single
/// request rather than paginating every PR the repo has ever had: that
/// unbounded walk cost one request per 100 PRs on every call and was the
/// call most likely to trip the rate limit. The `head.ref` check is kept as
/// a guard in case the filter is ever ignored.
pub fn find_existing(
    client: &Client,
    repo: &RepoId,
    branch: &str,
) -> Result<Option<PullRequest>, GitHubError> {
    let path = format!(
        "/repos/{}/{}/pulls?state=all&head={}&per_page=100",
        repo.owner,
        repo.repo,
        crate::github::encode_query(&format!("{}:{branch}", repo.owner))
    );
    let json = client.request("GET", &path, None)?;
    let prs: Vec<PullRequest> =
        serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))?;
    Ok(prs
        .into_iter()
        .find(|pr| pr.head.as_ref().is_some_and(|h| h.git_ref == branch)))
}

/// The `for item #<sequence_id> ` marker `pr_footer` stamps onto every PR
/// agentflare opens, shared by `find_by_item_marker`'s search query and
/// `marks_item`'s body check below.
fn item_marker(sequence_id: i64) -> String {
    format!("for item #{sequence_id} ")
}

/// True if `body` carries `item_marker(sequence_id)` -- i.e. this PR really
/// is `sequence_id`'s own, as opposed to an unrelated PR that only happens
/// to share the same branch name. Branch names get reused across items over
/// time, so a closed/merged `find_existing` match needs this confirmation
/// before a caller treats it as "this item's PR already exists" (item #63:
/// a stale, unrelated, already-merged PR from a prior item was returned as
/// the current item's `pr_url`, which made `in_review` true and skipped the
/// `nothing_was_ever_committed` safety net for real, uncommitted work).
pub fn marks_item(body: Option<&str>, sequence_id: i64) -> bool {
    body.is_some_and(|b| b.contains(&item_marker(sequence_id)))
}

const ITEM_ID_TAG_PREFIX: &str = "<!-- agentflare-item-id: ";

/// Hidden identity tag `pr_footer` appends after the visible marker. The
/// visible `for item #<sequence_id>` marker is only unique per project *and
/// per workstation* (each keeps its own local item database), so on its own
/// it can attribute one item's PR to a different item that happens to share
/// the same number (item #595: PR #636, opened for a different item #198,
/// nearly got this one's #198 auto-completed). The item's UUID is globally
/// unique.
pub fn item_id_tag(item_id: &str) -> String {
    format!("{ITEM_ID_TAG_PREFIX}{item_id} -->")
}

/// False when `body` carries *any* identity tag that is malformed (no closing
/// ` -->`) or names a *different* item than `item_id` -- every occurrence is
/// checked, so a matching tag can't launder a conflicting one after it. A body
/// with no tag at all (a PR opened before `item_id_tag` existed, or by hand)
/// still passes -- there's nothing to contradict, so callers fall back to the
/// sequence-number marker alone.
fn tag_allows(body: Option<&str>, item_id: &str) -> bool {
    let Some(mut rest) = body else {
        return true;
    };
    while let Some((_, after_prefix)) = rest.split_once(ITEM_ID_TAG_PREFIX) {
        let Some((tagged, after_tag)) = after_prefix.split_once(" -->") else {
            return false;
        };
        if tagged != item_id {
            return false;
        }
        rest = after_tag;
    }
    true
}

/// [`marks_item`] plus the identity-tag check: the PR must carry this
/// sequence number's marker *and* not be tagged for some other item.
pub fn marks_this_item(body: Option<&str>, sequence_id: i64, item_id: &str) -> bool {
    marks_item(body, sequence_id) && tag_allows(body, item_id)
}

/// True if `body` carries agentflare's own `for item #<N> via agentflare.`
/// stamp `pr_footer` puts on every PR it opens -- for *any* item, unlike
/// `marks_item` which checks one specific `sequence_id`. `discover_untracked_prs`
/// uses this: each workstation keeps its own local, unsynced item database
/// (see that function's doc comment), so a PR another workstation's
/// `push_and_open_pr` just opened for its own item is invisible to this
/// workstation's `known_pr_numbers` -- but the PR's body already carries this
/// stamp the instant it's created, regardless of which workstation opened it
/// or which local database (if any) is tracking it here. Without this check,
/// `discover_untracked_prs` raced a fresh `push_and_open_pr` creation on
/// another workstation and adopted the same PR into a second, duplicate local
/// item, stacking a second `beacon:` label on top of the opener's own (item
/// #261: PR #688 ended up with both `beacon:flared:51bb8de6c33b`, from the
/// PR's actual opener, and `beacon:flared:c997d745ae66`, from a second
/// workstation's discovery sweep 23 seconds later).
pub fn opened_by_agentflare(body: Option<&str>) -> bool {
    body.is_some_and(|b| b.contains("for item #") && b.contains(" via agentflare."))
}

/// Finds every PR (open, merged, or closed) whose body carries the
/// `for item #<sequence_id>` marker `pr_footer` stamps onto every PR
/// agentflare opens (see `push_and_open_pr`) -- the pre-dispatch
/// duplicate-work check (item #164). Unlike `find_existing`, this doesn't
/// depend on the item's own tracked branch name, so it still finds a PR
/// that merged while the item's tracked state fell out of sync (items
/// #122/#156: the state-side promotion never ran, so a routine redispatch
/// nearly re-did already-merged work).
///
/// GitHub's search API (used here as a candidate filter, mirroring the exact
/// `gh pr list --search "\"for item #N \" in:body"` query a human ran to
/// catch that incident) doesn't guarantee an exact phrase match -- it
/// tokenizes on punctuation like `#`, so a PR whose body merely mentions the
/// item nearby unrelated prose can surface as a false hit. PR #599's body
/// ("Motivated by two items (#184, #185 ...)") matched this way and got
/// item #184 auto-completed even though the PR never touched its code
/// (caught live, item #190). So each candidate's *actual* body is checked
/// locally afterward for the literal fixed suffix `pr_footer` always
/// stamps -- `for item #N via agentflare.` -- before it counts as a real
/// duplicate.
pub fn find_by_item_marker(
    client: &Client,
    repo: &RepoId,
    sequence_id: i64,
    item_id: &str,
) -> Result<Vec<PullRequest>, GitHubError> {
    let query = format!(
        "repo:{}/{} type:pr \"{}\" in:body",
        repo.owner,
        repo.repo,
        item_marker(sequence_id)
    );
    let path = format!("/search/issues?q={}", crate::github::encode_query(&query));
    let items = client.get_paginated(&path, search_items)?;
    let numbers: Vec<u64> = items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["number"].as_u64())
        .collect();
    let marker = format!("for item #{sequence_id} via agentflare.");
    let prs: Vec<PullRequest> = numbers
        .into_iter()
        .map(|n| get(client, repo, n))
        .collect::<Result<_, _>>()?;
    Ok(prs
        .into_iter()
        .filter(|pr| {
            pr.body.as_deref().is_some_and(|b| b.contains(&marker))
                && tag_allows(pr.body.as_deref(), item_id)
        })
        .collect())
}

pub fn get(client: &Client, repo: &RepoId, number: u64) -> Result<PullRequest, GitHubError> {
    let path = format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.repo);
    let json = client.request("GET", &path, None)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn merge(client: &Client, repo: &RepoId, number: u64, method: &str) -> Result<(), GitHubError> {
    merge_at_head(client, repo, number, method, None)
}

/// `merge`, pinned to `head_sha` when given: GitHub then refuses with 409
/// if the PR's head has moved since -- a commit pushed after the caller
/// judged CI green must never ride along into the merge unchecked. Callers
/// treat that 409 as "look again next tick" (see [`is_head_moved`]).
pub fn merge_at_head(
    client: &Client,
    repo: &RepoId,
    number: u64,
    method: &str,
    head_sha: Option<&str>,
) -> Result<(), GitHubError> {
    let path = format!("/repos/{}/{}/pulls/{number}/merge", repo.owner, repo.repo);
    let mut body = serde_json::json!({ "merge_method": method });
    if let Some(sha) = head_sha {
        body["sha"] = serde_json::Value::String(sha.to_string());
    }
    client.request("PUT", &path, Some(body))?;
    Ok(())
}

/// True when GitHub rejected a head-pinned `merge_at_head` because the PR's
/// head moved underneath the caller (409 Conflict). `update_branch`'s
/// equivalent mismatch comes back as a 422 naming the expected SHA.
pub fn is_head_moved(err: &GitHubError) -> bool {
    match err {
        GitHubError::Http { status: 409, .. } => true,
        GitHubError::Http { status: 422, body } => body.contains("expected head sha"),
        _ => false,
    }
}

/// Same server-side operation as the PR page's own "Update branch" button --
/// GitHub creates the merge commit bringing the base branch in, entirely on
/// its side, so this touches no local worktree/git state at all and can't
/// race a concurrently-dispatched job still pushing to the same branch the
/// way a local `git merge` would. Only ever called when the PR's
/// `mergeable_state` is already GitHub's own "behind" (mergeable, no
/// conflict) -- `worktree::run_review_sweep`'s job, not this function's, to
/// check that first.
///
/// `head_sha`, when given, is sent as GitHub's `expected_head_sha` guard:
/// the update is refused (422) instead of applied when the branch head is no
/// longer the one the caller saw, e.g. an agent pushed a fix in the meantime.
pub fn update_branch(
    client: &Client,
    repo: &RepoId,
    number: u64,
    head_sha: Option<&str>,
) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/pulls/{number}/update-branch",
        repo.owner, repo.repo
    );
    let body = head_sha.map(|sha| serde_json::json!({ "expected_head_sha": sha }));
    client.request("PUT", &path, body)?;
    Ok(())
}

pub fn comment(client: &Client, repo: &RepoId, number: u64, body: &str) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/issues/{number}/comments",
        repo.owner, repo.repo
    );
    client.request("POST", &path, Some(serde_json::json!({ "body": body })))?;
    Ok(())
}

pub fn request_review(
    client: &Client,
    repo: &RepoId,
    number: u64,
    reviewers: &[String],
) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/pulls/{number}/requested_reviewers",
        repo.owner, repo.repo
    );
    client.request(
        "POST",
        &path,
        Some(serde_json::json!({ "reviewers": reviewers })),
    )?;
    Ok(())
}

/// Review verdicts (approve/request-changes/comment) — includes bot reviewers
/// like CodeRabbit, which submit as ordinary PR reviews.
pub fn list_reviews(
    client: &Client,
    repo: &RepoId,
    number: u64,
) -> Result<Vec<Review>, GitHubError> {
    let path = format!("/repos/{}/{}/pulls/{number}/reviews", repo.owner, repo.repo);
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

/// Line-anchored review comments (diff comments), separate from general
/// issue-style comments returned by `issues::list_comments`. `since` (an
/// ISO8601 timestamp) filters to comments created after it — GitHub applies
/// the filter server-side, so passing the last-checked time keeps repeated
/// `pr_status` calls cheap.
pub fn list_review_comments(
    client: &Client,
    repo: &RepoId,
    number: u64,
    since: Option<&str>,
) -> Result<Vec<ReviewComment>, GitHubError> {
    let mut path = format!(
        "/repos/{}/{}/pulls/{number}/comments",
        repo.owner, repo.repo
    );
    if let Some(s) = since {
        path.push_str(&format!("?since={}", crate::github::encode_query(s)));
    }
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

/// Database IDs of review comments belonging to a *resolved* review thread.
/// REST has no resolution field at all (only GraphQL's `reviewThread.isResolved`
/// does), so this is a separate GraphQL call whose only job is to produce an
/// id set that `pr_status` filters the REST comment list against. The thread
/// query itself lives in `review_threads::list_review_threads` (paginated,
/// full comment context) since the supervisor's bot-thread follow-up needs
/// the same data.
pub fn resolved_review_comment_ids(
    client: &Client,
    repo: &RepoId,
    number: u64,
) -> Result<std::collections::HashSet<u64>, GitHubError> {
    crate::github::review_threads::resolved_review_comment_ids(client, repo, number)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::github::test_support::{MockResponse, MockServer};

    #[test]
    fn create_body_includes_optional_body_only_when_present() {
        let with = create_body("t", "h", "b", Some("desc"));
        assert_eq!(with["title"], "t");
        assert_eq!(with["body"], "desc");
        let without = create_body("t", "h", "b", None);
        assert!(without.get("body").is_none());
    }

    fn repo() -> RepoId {
        RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    #[test]
    fn create_posts_to_pulls_and_parses_the_response() {
        let server = MockServer::start(vec![MockResponse::json(
            201,
            r#"{"number":7,"html_url":"https://gh/o/r/pull/7","state":"open","title":"t"}"#,
        )]);
        let client = server.client(Some("tok"));
        let pr = create(&client, &repo(), "t", "head", "main", Some("desc")).unwrap();
        assert_eq!(pr.number, 7);

        let reqs = server.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/repos/o/r/pulls");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["head"], "head");
        assert_eq!(sent["base"], "main");
        assert_eq!(sent["body"], "desc");
    }

    #[test]
    fn list_encodes_state_in_the_query() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"number":1,"html_url":"u","state":"open","title":"a"}]"#,
        )]);
        let client = server.client(None);
        let prs = list(&client, &repo(), "open").unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/pulls?state=open&per_page=100&page=1"
        );
    }

    #[test]
    fn find_existing_returns_the_pr_matching_head_ref() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"number":5,"html_url":"https://gh/o/r/pull/5","state":"closed","title":"t","head":{"ref":"task/348","sha":"abc"}}]"#,
        )]);
        let client = server.client(None);
        let found = find_existing(&client, &repo(), "task/348").unwrap();
        assert_eq!(found.unwrap().number, 5);
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1, "one head-filtered request, no pagination");
        assert_eq!(
            reqs[0].path,
            "/repos/o/r/pulls?state=all&head=o%3Atask/348&per_page=100"
        );
    }

    #[test]
    fn find_existing_returns_none_when_no_pr_matches_the_branch() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"number":5,"html_url":"u","state":"open","title":"t","head":{"ref":"other-branch","sha":"abc"}}]"#,
        )]);
        let client = server.client(None);
        assert!(
            find_existing(&client, &repo(), "task/348")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn marks_item_true_when_body_carries_the_marker() {
        assert!(marks_item(
            Some("---\n_Opened by `claude-code` on **box** for item #63 via agentflare._"),
            63
        ));
    }

    #[test]
    fn marks_item_false_when_body_is_none() {
        assert!(!marks_item(None, 63));
    }

    #[test]
    fn marks_item_false_when_body_has_no_marker_at_all() {
        assert!(!marks_item(Some("just a regular PR description"), 63));
    }

    #[test]
    fn marks_item_does_not_let_a_shorter_id_match_a_longer_ones_marker() {
        // A PR marked "for item #63 " must not also count as evidence for
        // item #6 -- naive substring matching without the marker's own
        // digit-boundary delimiter would let "for item #6" match inside
        // "for item #63 ".
        assert!(!marks_item(
            Some("---\n_Opened by `claude-code` on **box** for item #63 via agentflare._"),
            6
        ));
    }

    #[test]
    fn marks_item_does_not_let_a_longer_id_match_a_shorter_ones_marker() {
        assert!(!marks_item(
            Some("---\n_Opened by `claude-code` on **box** for item #6 via agentflare._"),
            63
        ));
    }

    /// Item #595: two items in different databases share sequence #198; the
    /// visible marker alone can't tell their PRs apart, the identity tag can.
    #[test]
    fn marks_this_item_rejects_a_pr_tagged_for_a_different_item_with_the_same_number() {
        let body = format!(
            "---\n_Opened by `a` on **m** for item #198 via agentflare._\n{}",
            item_id_tag("uuid-of-the-review-sweep-item")
        );
        assert!(!marks_this_item(
            Some(&body),
            198,
            "uuid-of-the-gateway-item"
        ));
        assert!(marks_this_item(
            Some(&body),
            198,
            "uuid-of-the-review-sweep-item"
        ));
    }

    /// Item #595's incident shape end to end: two PRs both stamped "for item
    /// #198" (one from another item's database, one this item's own) --
    /// only the one whose identity tag doesn't contradict this item survives.
    #[test]
    fn find_by_item_marker_drops_a_pr_tagged_for_a_different_item() {
        let footer = |uuid: &str| {
            format!(
                "_Opened by `a` on **m** for item #198 via agentflare._\n{}",
                item_id_tag(uuid)
            )
        };
        let pr_json = |number: u64, body: &str| {
            serde_json::json!({
                "number": number, "html_url": "u", "state": "closed", "title": "t",
                "merged_at": "2026-09-18T00:00:00Z", "body": body
            })
            .to_string()
        };
        let server = MockServer::start(vec![
            MockResponse::json(200, r#"{"items":[{"number":636},{"number":637}]}"#),
            MockResponse::json(200, &pr_json(636, &footer("review-sweep-item-uuid"))),
            MockResponse::json(200, &pr_json(637, &footer("gateway-item-uuid"))),
        ]);
        let client = server.client(None);
        let prs = find_by_item_marker(&client, &repo(), 198, "gateway-item-uuid").unwrap();
        assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), vec![637]);
    }

    /// Every tag occurrence counts: a matching tag followed by a conflicting or
    /// malformed one is an ambiguous identity, not a match.
    #[test]
    fn marks_this_item_rejects_a_body_with_a_second_conflicting_or_malformed_tag() {
        let marker = "_Opened by `a` on **m** for item #198 via agentflare._";
        let ok = item_id_tag("mine");
        let conflicting = format!("{marker}\n{ok}\n{}", item_id_tag("other"));
        assert!(!marks_this_item(Some(&conflicting), 198, "mine"));
        let malformed = format!("{marker}\n{ok}\n{ITEM_ID_TAG_PREFIX}mine");
        assert!(!marks_this_item(Some(&malformed), 198, "mine"));
        let repeated = format!("{marker}\n{ok}\n{ok}");
        assert!(marks_this_item(Some(&repeated), 198, "mine"));
    }

    #[test]
    fn marks_this_item_falls_back_to_the_number_for_an_untagged_legacy_pr() {
        let body = "---\n_Opened by `a` on **m** for item #198 via agentflare._";
        assert!(marks_this_item(Some(body), 198, "any-uuid"));
        assert!(!marks_this_item(Some(body), 199, "any-uuid"));
    }

    #[test]
    fn opened_by_agentflare_true_for_any_items_marker() {
        assert!(opened_by_agentflare(Some(
            "---\n_Opened by `claude-code` on **flared:51bb8de6c33b** for item #259 via agentflare._"
        )));
    }

    #[test]
    fn opened_by_agentflare_false_when_body_is_none() {
        assert!(!opened_by_agentflare(None));
    }

    #[test]
    fn opened_by_agentflare_false_for_a_hand_opened_pr() {
        assert!(!opened_by_agentflare(Some("just a regular PR description")));
    }

    #[test]
    fn find_by_item_marker_searches_and_fetches_each_matching_pr() {
        let server = MockServer::start(vec![
            MockResponse::json(200, r#"{"items":[{"number":42}]}"#),
            MockResponse::json(
                200,
                r#"{"number":42,"html_url":"u","state":"closed","title":"t","merged_at":"2026-08-01T00:00:00Z","body":"---\n_Opened by `claude-code` on **box** for item #164 via agentflare._"}"#,
            ),
        ]);
        let client = server.client(None);
        let found = find_by_item_marker(&client, &repo(), 164, "item-uuid").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].number, 42);
        assert!(found[0].merged_at.is_some());

        let reqs = server.requests();
        assert_eq!(reqs[0].method, "GET");
        assert!(reqs[0].path.starts_with("/search/issues?q="));
        assert!(reqs[0].path.contains("repo%3Ao/r"));
        assert!(reqs[0].path.contains("%22for%20item%20%23164%20%22"));
        assert_eq!(reqs[1].path, "/repos/o/r/pulls/42");
    }

    #[test]
    fn find_by_item_marker_returns_empty_when_search_finds_nothing() {
        let server = MockServer::start(vec![MockResponse::json(200, r#"{"items":[]}"#)]);
        let client = server.client(None);
        assert!(
            find_by_item_marker(&client, &repo(), 999, "item-uuid")
                .unwrap()
                .is_empty()
        );
    }

    // Regression for item #190: GitHub's search API matched PR #599's body
    // ("Motivated by two items (#184, #185 ...)") against the `"for item
    // #184 " in:body` query even though the PR never carries the actual
    // `pr_footer` stamp for #184, which nearly got #184 auto-completed as
    // a false-positive duplicate.
    #[test]
    fn find_by_item_marker_drops_a_search_hit_that_lacks_the_literal_footer() {
        let server = MockServer::start(vec![
            MockResponse::json(200, r#"{"items":[{"number":599}]}"#),
            MockResponse::json(
                200,
                r#"{"number":599,"html_url":"u","state":"closed","title":"t","body":"Motivated by two items (#184, #185 in the linked project) for item #184 discovery."}"#,
            ),
        ]);
        let client = server.client(None);
        assert!(
            find_by_item_marker(&client, &repo(), 184, "item-uuid")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn get_fetches_a_single_pull() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"number":9,"html_url":"u","state":"closed","title":"x"}"#,
        )]);
        let client = server.client(None);
        let pr = get(&client, &repo(), 9).unwrap();
        assert_eq!(pr.state, "closed");
        assert_eq!(server.requests()[0].path, "/repos/o/r/pulls/9");
    }

    #[test]
    fn merge_puts_the_chosen_method() {
        let server = MockServer::start(vec![MockResponse::json(200, r#"{"merged":true}"#)]);
        let client = server.client(Some("tok"));
        merge(&client, &repo(), 3, "squash").unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "PUT");
        assert_eq!(reqs[0].path, "/repos/o/r/pulls/3/merge");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["merge_method"], "squash");
    }

    #[test]
    fn merge_at_head_pins_the_checked_sha() {
        let server = MockServer::start(vec![MockResponse::json(200, r#"{"merged":true}"#)]);
        let client = server.client(Some("tok"));
        merge_at_head(&client, &repo(), 3, "squash", Some("abc123")).unwrap();
        let sent: serde_json::Value = serde_json::from_str(&server.requests()[0].body).unwrap();
        assert_eq!(sent["sha"], "abc123");
        assert_eq!(sent["merge_method"], "squash");
    }

    #[test]
    fn merge_at_head_reports_a_moved_head_as_is_head_moved() {
        let server = MockServer::start(vec![MockResponse::json(
            409,
            r#"{"message":"Head branch was modified. Review and try the merge again."}"#,
        )]);
        let client = server.client(Some("tok"));
        let err = merge_at_head(&client, &repo(), 3, "squash", Some("abc123")).unwrap_err();
        assert!(is_head_moved(&err));
        assert!(!is_head_moved(&GitHubError::Http {
            status: 405,
            body: "not mergeable".into()
        }));
    }

    #[test]
    fn update_branch_sends_expected_head_sha() {
        let server = MockServer::start(vec![MockResponse::json(202, r#"{"message":"Updating"}"#)]);
        let client = server.client(Some("tok"));
        update_branch(&client, &repo(), 7, Some("def456")).unwrap();
        let sent: serde_json::Value = serde_json::from_str(&server.requests()[0].body).unwrap();
        assert_eq!(sent["expected_head_sha"], "def456");
    }

    #[test]
    fn update_branch_puts_to_the_update_branch_endpoint() {
        let server = MockServer::start(vec![MockResponse::json(202, r#"{"message":"Updating"}"#)]);
        let client = server.client(Some("tok"));
        update_branch(&client, &repo(), 7, None).unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "PUT");
        assert_eq!(reqs[0].path, "/repos/o/r/pulls/7/update-branch");
    }

    #[test]
    fn comment_posts_to_the_issues_comments_endpoint() {
        let server = MockServer::start(vec![MockResponse::json(201, r#"{"id":1}"#)]);
        let client = server.client(Some("tok"));
        comment(&client, &repo(), 5, "hello").unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/repos/o/r/issues/5/comments");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["body"], "hello");
    }

    #[test]
    fn request_review_sends_the_reviewer_list() {
        let server = MockServer::start(vec![MockResponse::json(201, r#"{"id":1}"#)]);
        let client = server.client(Some("tok"));
        request_review(&client, &repo(), 8, &["alice".into(), "bob".into()]).unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/repos/o/r/pulls/8/requested_reviewers");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["reviewers"][0], "alice");
        assert_eq!(sent["reviewers"][1], "bob");
    }

    #[test]
    fn list_reviews_fetches_and_parses() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"user":{"login":"coderabbitai[bot]"},"state":"APPROVED","body":"lgtm","submitted_at":"2026-07-19T00:00:00Z"}]"#,
        )]);
        let client = server.client(None);
        let reviews = list_reviews(&client, &repo(), 5).unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].user.login, "coderabbitai[bot]");
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/pulls/5/reviews?per_page=100&page=1"
        );
    }

    #[test]
    fn list_review_comments_fetches_and_parses() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"id":1,"user":{"login":"bob"},"path":"src/x.rs","line":42,"body":"nit"}]"#,
        )]);
        let client = server.client(None);
        let comments = list_review_comments(&client, &repo(), 5, None).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].line, Some(42));
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/pulls/5/comments?per_page=100&page=1"
        );
    }

    #[test]
    fn list_review_comments_appends_since_query() {
        let server = MockServer::start(vec![MockResponse::json(200, "[]")]);
        let client = server.client(None);
        list_review_comments(&client, &repo(), 5, Some("2026-07-19T00:00:00Z")).unwrap();
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/pulls/5/comments?since=2026-07-19T00%3A00%3A00Z&per_page=100&page=1"
        );
    }

    #[test]
    fn resolved_review_comment_ids_collects_ids_from_resolved_threads_only() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
                {"isResolved":true,"comments":{"nodes":[{"databaseId":1},{"databaseId":2}]}},
                {"isResolved":false,"comments":{"nodes":[{"databaseId":3}]}}
            ]}}}}}"#,
        )]);
        let client = server.client(Some("tok"));
        let ids = resolved_review_comment_ids(&client, &repo(), 5).unwrap();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
        assert!(!ids.contains(&3));
        assert_eq!(server.requests()[0].path, "/graphql");
    }

    #[test]
    fn resolved_review_comment_ids_surfaces_graphql_errors() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"errors":[{"message":"Could not resolve to a PullRequest"}]}"#,
        )]);
        let client = server.client(Some("tok"));
        let err = resolved_review_comment_ids(&client, &repo(), 5).unwrap_err();
        assert!(matches!(err, GitHubError::Parse(_)));
    }
}
