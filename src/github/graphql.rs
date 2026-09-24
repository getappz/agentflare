//! Batched GraphQL PR/CI lookups for `supervisor::run_review_sweep`. A serial
//! sweep issuing 1-2 REST calls per in-review item costs ~500-1,200 calls a
//! tick at "a few hundred PRs across 10-15 projects" -- enough to exhaust the
//! shared token's 5,000 requests/hour budget in well under 15 minutes of
//! continuous ticking. This collapses that into a handful of GraphQL calls by
//! aliasing every PR lookup into one query, reusing the exact
//! `client.request("POST", "/graphql", ...)` pattern
//! `pulls::resolved_review_comment_ids` already established.
use std::collections::HashMap;

use crate::github::models::CheckRun;
use crate::github::{Client, GitHubError, RepoId};

/// Max PRs aliased into a single GraphQL query. GitHub prices a GraphQL call
/// by the number of nodes it touches, not by call count -- each aliased PR
/// here pulls in up to ~120 nested nodes (100 check contexts + 20 labels), so
/// a 40-PR query costs on the order of tens of rate-limit points, safely
/// under the 5,000 points/hour budget even across several large sweeps an
/// hour. Chosen well below GitHub's harder per-query node-count ceiling
/// rather than against it, so one oversized project can't itself blow the
/// budget the batching is meant to protect.
pub const GRAPHQL_PR_BATCH_SIZE: usize = 40;

/// Everything `worktree::pr_ci_status_impl`'s REST path fetches per PR,
/// gathered by one aliased GraphQL sub-query instead. Kept separate from
/// `models::PullRequest` since the two APIs represent a couple of these
/// fields differently (GraphQL's `mergeable`/`mergeStateStatus` enums vs
/// REST's bool/string) and normalizing them into REST's shape here is what
/// lets `worktree::pr_ci_status_from_batch` share its decision logic with the
/// REST path unchanged.
#[derive(Debug)]
pub struct BatchPrData {
    pub merged: bool,
    /// GraphQL `state == CLOSED` -- closed without merging (a merged PR
    /// reports `MERGED` instead, so this and `merged` are never both true).
    pub closed: bool,
    pub mergeable: Option<bool>,
    /// Lowercased to match REST's `mergeable_state` string values ("behind",
    /// "clean", ...) -- GraphQL's `mergeStateStatus` enum comes back
    /// upper-cased (`BEHIND`).
    pub mergeable_state: Option<String>,
    /// Both check runs and legacy commit statuses (`StatusContext`s, folded
    /// in via `CheckRun::from_status`), each flagged with whether branch
    /// protection requires it for this PR.
    pub checks: Vec<CheckRun>,
    pub labels: Vec<String>,
    /// `headRefOid`: the commit these checks ran against. Carried through to
    /// the merge/update-branch calls so they act on exactly the head this
    /// snapshot judged, not whatever was pushed since.
    pub head_sha: Option<String>,
    /// `reviewDecision`, upper-case as GitHub sends it (`APPROVED`,
    /// `REVIEW_REQUIRED`, `CHANGES_REQUESTED`), or `None` when the repo
    /// requires no review.
    pub review_decision: Option<String>,
    /// `statusCheckRollup.state` (upper-case), GitHub's own roll-up over
    /// every context on the head commit -- consulted when the context list
    /// itself came back empty.
    pub rollup_state: Option<String>,
}

/// One aliased sub-query for PR `number` -- `pr<number>` is a valid GraphQL
/// alias (starts with a letter) and unique per number within one query, so
/// results can be matched back up by parsing the alias GitHub echoes back in
/// its response rather than needing a second lookup table.
fn pr_alias(number: u64) -> String {
    format!("pr{number}")
}

fn pr_subquery(number: u64) -> String {
    format!(
        "{}: pullRequest(number: {number}) {{ state merged mergeable mergeStateStatus \
         reviewDecision headRefOid \
         labels(first: 20) {{ nodes {{ name }} }} \
         commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state contexts(first: 100) {{ \
         nodes {{ __typename \
         ... on CheckRun {{ name status conclusion isRequired(pullRequestNumber: {number}) }} \
         ... on StatusContext {{ context state isRequired(pullRequestNumber: {number}) }} \
         }} }} }} }} }} }} }}",
        pr_alias(number)
    )
}

/// Maps a GraphQL response's `errors` array to a `GitHubError`: GitHub
/// reports an exhausted GraphQL budget as a 200 whose errors carry
/// `"type": "RATE_LIMITED"`, which must read as `RateLimited` (so callers
/// back off) rather than as a malformed response.
pub(crate) fn graphql_error(errors: &serde_json::Value) -> GitHubError {
    let rate_limited = errors
        .as_array()
        .is_some_and(|errs| errs.iter().any(|e| e["type"] == "RATE_LIMITED"));
    if rate_limited {
        GitHubError::RateLimited(format!("GitHub GraphQL rate limit hit: {errors}"))
    } else {
        GitHubError::Parse(format!("GraphQL error: {errors}"))
    }
}

/// Fetches `BatchPrData` for every PR in `numbers` in one GraphQL request.
/// Callers with more than `GRAPHQL_PR_BATCH_SIZE` numbers must chunk first --
/// this function does not, so its own cost stays predictable and testable
/// per call.
pub fn batch_pr_status(
    client: &Client,
    repo: &RepoId,
    numbers: &[u64],
) -> Result<HashMap<u64, BatchPrData>, GitHubError> {
    let mut out = HashMap::new();
    if numbers.is_empty() {
        return Ok(out);
    }
    let fields: String = numbers.iter().map(|n| pr_subquery(*n)).collect();
    let query = format!(
        "query($owner:String!,$repo:String!){{repository(owner:$owner,name:$repo){{{fields}}}}}"
    );
    let body = serde_json::json!({
        "query": query,
        "variables": { "owner": repo.owner, "repo": repo.repo }
    });
    let json = client.request("POST", "/graphql", Some(body))?;
    if let Some(errors) = json.get("errors") {
        return Err(graphql_error(errors));
    }
    let Some(repository) = json.get("data").and_then(|d| d.get("repository")) else {
        return Err(GitHubError::Parse(
            "GraphQL response missing data.repository".to_string(),
        ));
    };
    for number in numbers {
        // A null/missing alias means GitHub couldn't resolve that PR number
        // (deleted, or a repo mismatch) -- left out of the map so the caller
        // treats it the same as any other "couldn't determine status" case
        // rather than as a parse error for the whole batch.
        match repository.get(pr_alias(*number)) {
            Some(node) if !node.is_null() => {
                out.insert(*number, parse_batch_pr(node));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Soft-fails per chunk rather than failing the whole sweep: a transient
/// GraphQL error on one chunk of PRs must not blind the sweep to every other
/// chunk's (possibly actionable) results, mirroring the existing per-item
/// soft-fail stance in `worktree::pr_ci_status_impl` for a single REST
/// failure.
pub fn batch_pr_status_chunked(
    client: &Client,
    repo: &RepoId,
    numbers: &[u64],
) -> HashMap<u64, BatchPrData> {
    let mut out = HashMap::new();
    for chunk in numbers.chunks(GRAPHQL_PR_BATCH_SIZE) {
        match batch_pr_status(client, repo, chunk) {
            Ok(map) => out.extend(map),
            Err(e) => eprintln!(
                "github: batch PR status GraphQL query failed for {} PR(s) in {repo}: {e}",
                chunk.len()
            ),
        }
    }
    out
}

fn parse_batch_pr(node: &serde_json::Value) -> BatchPrData {
    let merged = node["merged"].as_bool().unwrap_or(false);
    let closed = !merged && node["state"].as_str() == Some("CLOSED");
    let mergeable = match node["mergeable"].as_str() {
        Some("MERGEABLE") => Some(true),
        Some("CONFLICTING") => Some(false),
        _ => None,
    };
    let mergeable_state = node["mergeStateStatus"].as_str().map(str::to_lowercase);
    let labels = node["labels"]["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|l| l["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let rollup = &node["commits"]["nodes"][0]["commit"]["statusCheckRollup"];
    let checks = rollup["contexts"]["nodes"]
        .as_array()
        .map(|nodes| nodes.iter().filter_map(parse_context).collect())
        .unwrap_or_default();
    let string = |v: &serde_json::Value| v.as_str().map(str::to_string);
    BatchPrData {
        merged,
        closed,
        mergeable,
        mergeable_state,
        checks,
        labels,
        head_sha: string(&node["headRefOid"]),
        review_decision: string(&node["reviewDecision"]),
        rollup_state: string(&rollup["state"]),
    }
}

/// One `statusCheckRollup` context: a Checks-API `CheckRun`, or a legacy
/// Statuses-API `StatusContext` (third-party CI, CLA bots) -- both gate a
/// merge the same way, so both count.
fn parse_context(c: &serde_json::Value) -> Option<CheckRun> {
    let required = c["isRequired"].as_bool().unwrap_or(false);
    match c["__typename"].as_str()? {
        "CheckRun" => Some(CheckRun {
            name: c["name"].as_str()?.to_string(),
            status: c["status"].as_str().unwrap_or_default().to_lowercase(),
            conclusion: c["conclusion"].as_str().map(str::to_lowercase),
            required,
        }),
        "StatusContext" => Some(CheckRun::from_status(
            c["context"].as_str()?,
            c["state"].as_str().unwrap_or_default(),
            required,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::test_support::{MockResponse, MockServer};

    fn repo() -> RepoId {
        RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    #[test]
    fn batch_pr_status_returns_empty_map_without_a_network_call() {
        let server = MockServer::start(vec![]);
        let client = server.client(Some("tok"));
        let out = batch_pr_status(&client, &repo(), &[]).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn batch_pr_status_aliases_every_number_into_one_query() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"repository":{
                "pr101":{"merged":true,"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","labels":{"nodes":[]},"commits":{"nodes":[]}},
                "pr102":{"merged":false,"mergeable":"MERGEABLE","mergeStateStatus":"BEHIND","labels":{"nodes":[]},"commits":{"nodes":[]}}
            }}}"#,
        )]);
        let client = server.client(Some("tok"));
        let out = batch_pr_status(&client, &repo(), &[101, 102]).unwrap();

        assert!(out[&101].merged);
        assert!(!out[&102].merged);
        assert_eq!(out[&102].mergeable_state.as_deref(), Some("behind"));

        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/graphql");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert!(
            sent["query"]
                .as_str()
                .unwrap()
                .contains("pr101: pullRequest(number: 101)")
        );
        assert!(
            sent["query"]
                .as_str()
                .unwrap()
                .contains("pr102: pullRequest(number: 102)")
        );
    }

    #[test]
    fn batch_pr_status_extracts_check_runs_and_lowercases_graphql_enums() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"repository":{"pr7":{
                "merged":false,"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                "labels":{"nodes":[{"name":"status:pr:approved"}]},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
                    {"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"SUCCESS"},
                    {"__typename":"CheckRun","name":"clippy","status":"COMPLETED","conclusion":"FAILURE"},
                    {"__typename":"StatusContext","context":"legacy-ci","state":"SUCCESS"}
                ]}}}}]}
            }}}}"#,
        )]);
        let client = server.client(Some("tok"));
        let out = batch_pr_status(&client, &repo(), &[7]).unwrap();
        let pr = &out[&7];

        assert_eq!(pr.labels, vec!["status:pr:approved".to_string()]);
        // The legacy StatusContext counts too -- a third-party CI or CLA bot
        // reporting through the Statuses API gates a merge just the same.
        assert_eq!(pr.checks.len(), 3);
        assert_eq!(pr.checks[0].name, "build");
        assert_eq!(pr.checks[0].status, "completed");
        assert_eq!(pr.checks[0].conclusion.as_deref(), Some("success"));
        assert_eq!(pr.checks[1].conclusion.as_deref(), Some("failure"));
        assert_eq!(pr.checks[2].name, "legacy-ci");
        assert_eq!(pr.checks[2].status, "completed");
        assert_eq!(pr.checks[2].conclusion.as_deref(), Some("success"));
    }

    #[test]
    fn batch_pr_status_reads_head_sha_review_decision_rollup_and_required_flags() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"repository":{"pr8":{
                "merged":false,"mergeable":"MERGEABLE","mergeStateStatus":"BLOCKED",
                "reviewDecision":"REVIEW_REQUIRED","headRefOid":"abc123",
                "labels":{"nodes":[]},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"PENDING","contexts":{"nodes":[
                    {"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"SUCCESS","isRequired":true},
                    {"__typename":"StatusContext","context":"deploy-preview","state":"PENDING","isRequired":false}
                ]}}}}]}
            }}}}"#,
        )]);
        let client = server.client(Some("tok"));
        let out = batch_pr_status(&client, &repo(), &[8]).unwrap();
        let pr = &out[&8];
        assert_eq!(pr.head_sha.as_deref(), Some("abc123"));
        assert_eq!(pr.review_decision.as_deref(), Some("REVIEW_REQUIRED"));
        assert_eq!(pr.rollup_state.as_deref(), Some("PENDING"));
        assert!(pr.checks[0].required);
        assert!(!pr.checks[1].required);
        assert_eq!(pr.checks[1].status, "pending");

        let reqs = server.requests();
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        let query = sent["query"].as_str().unwrap();
        for field in [
            "headRefOid",
            "reviewDecision",
            "statusCheckRollup { state",
            "... on StatusContext { context state isRequired(pullRequestNumber: 8) }",
            "isRequired(pullRequestNumber: 8)",
        ] {
            assert!(query.contains(field), "query must request {field}: {query}");
        }
    }

    #[test]
    fn batch_pr_status_maps_graphql_rate_limited_errors_to_rate_limited() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#,
        )]);
        let client = server.client(Some("tok"));
        let err = batch_pr_status(&client, &repo(), &[1]).unwrap_err();
        assert!(matches!(err, GitHubError::RateLimited(_)));
    }

    #[test]
    fn batch_pr_status_treats_a_missing_alias_as_absent_not_an_error() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"repository":{"pr9":null}}}"#,
        )]);
        let client = server.client(Some("tok"));
        let out = batch_pr_status(&client, &repo(), &[9]).unwrap();
        assert!(!out.contains_key(&9));
    }

    #[test]
    fn batch_pr_status_surfaces_graphql_errors() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"errors":[{"message":"rate limited"}]}"#,
        )]);
        let client = server.client(Some("tok"));
        let err = batch_pr_status(&client, &repo(), &[1]).unwrap_err();
        assert!(matches!(err, GitHubError::Parse(_)));
    }

    #[test]
    fn batch_pr_status_chunked_merges_a_single_chunks_results() {
        // Two PR numbers both fit under `GRAPHQL_PR_BATCH_SIZE`, so this is
        // one chunk/one request -- the multi-chunk split itself is exercised
        // by `batch_pr_status_chunked_issues_one_query_per_chunk` below via
        // a number count that spans two chunks.
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"repository":{
                "pr1":{"merged":true,"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","labels":{"nodes":[]},"commits":{"nodes":[]}},
                "pr2":{"merged":false,"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","labels":{"nodes":[]},"commits":{"nodes":[]}}
            }}}"#,
        )]);
        let client = server.client(Some("tok"));
        let out = batch_pr_status_chunked(&client, &repo(), &[1, 2]);
        assert!(out[&1].merged);
        assert!(!out[&2].merged);
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn batch_pr_status_chunked_issues_one_query_per_chunk() {
        let numbers: Vec<u64> = (1..=(GRAPHQL_PR_BATCH_SIZE as u64 + 1)).collect();
        let server = MockServer::start(vec![
            MockResponse::json(200, r#"{"data":{"repository":{}}}"#),
            MockResponse::json(200, r#"{"data":{"repository":{}}}"#),
        ]);
        let client = server.client(Some("tok"));
        batch_pr_status_chunked(&client, &repo(), &numbers);
        assert_eq!(
            server.requests().len(),
            2,
            "{} numbers must split into two chunks of at most {GRAPHQL_PR_BATCH_SIZE}",
            numbers.len()
        );
    }

    #[test]
    fn batch_pr_status_chunked_keeps_other_chunks_results_when_one_chunk_errors() {
        let server = MockServer::start(vec![MockResponse::json(
            500,
            r#"{"message":"server error"}"#,
        )]);
        let client = server.client(Some("tok"));
        let out = batch_pr_status_chunked(&client, &repo(), &[1]);
        assert!(out.is_empty());
    }
}
