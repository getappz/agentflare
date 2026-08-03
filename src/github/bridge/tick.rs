//! One poll cycle. Ordering matters: re-verify (and cede) BEFORE doing new
//! work, so a lost race is dropped as early as possible; claim LAST, so
//! headroom reflects everything already taken on this tick.

use crate::github::bridge::claim as claim_rules;
use crate::github::bridge::config::{BridgeConfig, CLAIMED_LABEL_PREFIX};
use crate::github::bridge::items;
use crate::github::bridge::marker::{Action, Marker, content_hash};
use crate::github::models::Issue;
use crate::github::{Client, GitHubError, RepoId, issues};

#[allow(dead_code)] // consumer arrives in a later bridge task
pub struct Ctx {
    pub client: Client,
    pub repo: RepoId,
    pub config: BridgeConfig,
    pub project_id: String,
    /// The claim ledger lives in a DIFFERENT database from items:
    /// `crate::db::open()` (agentflare.db) vs the backend db passed to
    /// `run_once`. Passing the backend connection to `claims::*` fails with
    /// "no such table: claims" — and because those calls are best-effort,
    /// it would fail SILENTLY, leaving Layer 1 dead. Keep them separate.
    pub ledger: rusqlite::Connection,
}

#[derive(Debug, Default, PartialEq, Eq)]
#[allow(dead_code)] // consumer arrives in a later bridge task
pub struct TickReport {
    pub claimed: Vec<u64>,
    pub ceded: Vec<u64>,
    pub imported: Vec<u64>,
    pub exported: Vec<u64>,
}

/// Replaces any existing marker footer rather than stacking a new one, so an
/// issue body never accumulates markers across ticks.
#[allow(dead_code)] // consumer arrives in a later bridge task
pub fn issue_body_with_marker(body: &str, marker: &Marker) -> String {
    let open = format!("<!-- {} ", crate::github::bridge::marker::MARKER_VERSION);
    let base = match body.rfind(&open) {
        Some(i) => body[..i].trim_end(),
        None => body.trim_end(),
    };
    if base.is_empty() {
        marker.render()
    } else {
        format!("{base}\n\n{}", marker.render())
    }
}

/// Rate limiting and auth failure are expected operating conditions, not bugs:
/// they end the tick quietly so the daemon keeps looping and retries later.
#[allow(dead_code)] // consumer arrives in a later bridge task
fn is_soft(err: &GitHubError) -> bool {
    matches!(
        err,
        GitHubError::RateLimited(_) | GitHubError::NoAuth(_) | GitHubError::Forbidden(_)
    )
}

#[allow(dead_code)] // consumer arrives in a later bridge task
fn comment_pairs(ctx: &Ctx, number: u64) -> Result<Vec<(u64, String)>, GitHubError> {
    Ok(issues::list_comments(&ctx.client, &ctx.repo, number, None)?
        .into_iter()
        .map(|c| (c.id, c.body))
        .collect())
}

#[allow(dead_code)] // consumer arrives in a later bridge task
fn marker_for(ctx: &Ctx, action: Action, item_id: &str, hash: &str, now: i64) -> Marker {
    Marker {
        action,
        owner: ctx.config.instance_id.clone(),
        item: item_id.to_string(),
        ts: now,
        hash: hash.to_string(),
    }
}

#[allow(dead_code)] // consumer arrives in a later bridge task
pub fn run_once(ctx: &Ctx, conn: &rusqlite::Connection, now: i64) -> Result<TickReport, String> {
    let mut report = TickReport::default();
    match run_inner(ctx, conn, now, &mut report) {
        Ok(()) => Ok(report),
        // Soft errors keep whatever the tick already accomplished.
        Err(e) if is_soft(&e) => Ok(report),
        Err(e) => Err(e.to_string()),
    }
}

#[allow(dead_code)] // consumer arrives in a later bridge task
fn run_inner(
    ctx: &Ctx,
    conn: &rusqlite::Connection,
    now: i64,
    report: &mut TickReport,
) -> Result<(), GitHubError> {
    let queued = issues::list_filtered(
        &ctx.client,
        &ctx.repo,
        "open",
        Some(&ctx.config.queue_label),
        None,
    )?;

    // 1. Re-verify claims we believe we hold; cede any we actually lost.
    for issue in &queued {
        let Some(item) = items::find_by_issue(conn, &ctx.project_id, issue.number) else {
            continue;
        };
        let comments = comment_pairs(ctx, issue.number)?;
        if claim_rules::i_hold(&comments, &ctx.config.instance_id, now, ctx.config.ttl_secs) {
            continue;
        }
        cede(ctx, conn, issue.number, &item, now)?;
        report.ceded.push(issue.number);
    }

    // 2. Export dirty items we still hold.
    for issue in &queued {
        if report.ceded.contains(&issue.number) {
            continue;
        }
        let Some(item) = items::find_by_issue(conn, &ctx.project_id, issue.number) else {
            continue;
        };
        if export_if_dirty(ctx, conn, issue, &item, now)? {
            report.exported.push(issue.number);
        }
    }

    // 3. Claim new work, up to remaining headroom.
    let held = held_count(conn, ctx);
    let headroom = ctx.config.max_claims.saturating_sub(held);
    for issue in queued.iter().take(headroom) {
        if items::find_by_issue(conn, &ctx.project_id, issue.number).is_some() {
            continue;
        }
        if try_claim(ctx, conn, issue, now)? {
            report.claimed.push(issue.number);
        }
    }
    Ok(())
}

#[allow(dead_code)] // consumer arrives in a later bridge task
fn held_count(conn: &rusqlite::Connection, ctx: &Ctx) -> usize {
    items_tracked(conn, ctx).len()
}

#[allow(dead_code)] // consumer arrives in a later bridge task
fn items_tracked(conn: &rusqlite::Connection, ctx: &Ctx) -> Vec<agentflare_backend::item::Item> {
    agentflare_backend::item::list_by_project(conn, &ctx.project_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.external_source.as_deref() == Some(items::EXTERNAL_SOURCE))
        .filter(|i| i.completed_at.is_none())
        .collect()
}

/// Optimistic two-step claim: post our marker, then re-read and check we are
/// the earliest. GitHub offers no compare-and-swap on labels or comments, so
/// this is the closest available approximation.
#[allow(dead_code)] // consumer arrives in a later bridge task
fn try_claim(
    ctx: &Ctx,
    conn: &rusqlite::Connection,
    issue: &Issue,
    now: i64,
) -> Result<bool, GitHubError> {
    let before = comment_pairs(ctx, issue.number)?;
    if claim_rules::resolve_holder(&before, now, ctx.config.ttl_secs).is_some() {
        return Ok(false); // already held by someone live
    }

    let hash = issue_hash(issue);
    let marker = marker_for(ctx, Action::Claim, "pending", &hash, now);
    issues::comment(
        &ctx.client,
        &ctx.repo,
        issue.number,
        &format!(
            "Claiming this for `{}`.\n\n{}",
            ctx.config.instance_id,
            marker.render()
        ),
    )?;

    let after = comment_pairs(ctx, issue.number)?;
    if !claim_rules::i_hold(&after, &ctx.config.instance_id, now, ctx.config.ttl_secs) {
        return Ok(false); // lost the race; leave no local trace
    }

    let Some(state_id) = items::state_id_for_group(conn, &ctx.project_id, "started") else {
        return Ok(false);
    };
    let created = agentflare_backend::item::create(
        conn,
        agentflare_backend::item::CreateItem {
            project_id: ctx.project_id.clone(),
            state_id,
            name: issue.title.clone(),
            description: issue.body.clone(),
            priority: None,
            parent_id: None,
            assignee_agent: Some(ctx.config.instance_id.clone()),
            sort_order: None,
            external_source: Some(items::EXTERNAL_SOURCE.to_string()),
            external_id: Some(issue.number.to_string()),
            metadata: None,
            label_ids: vec![],
            assignee_ids: vec![],
            dependency_ids: vec![],
        },
    )
    .map_err(|e| GitHubError::Parse(e.to_string()))?;

    // Local ledger too, so this instance's OWN agents do not double-claim.
    // NOTE: `&ctx.ledger`, NOT `conn` — separate databases.
    let _ = crate::claims::acquire(
        &ctx.ledger,
        &ctx.repo.to_string(),
        &format!("issue#{}", issue.number),
        &ctx.config.instance_id,
        None,
        None,
        now,
        ctx.config.ttl_secs,
    );

    debug_assert_eq!(
        created.external_id.as_deref(),
        Some(issue.number.to_string().as_str())
    );
    issues::add_labels(
        &ctx.client,
        &ctx.repo,
        issue.number,
        &[format!("{CLAIMED_LABEL_PREFIX}{}", ctx.config.instance_id)],
    )?;
    Ok(true)
}

#[allow(dead_code)] // consumer arrives in a later bridge task
fn cede(
    ctx: &Ctx,
    conn: &rusqlite::Connection,
    number: u64,
    item: &agentflare_backend::item::Item,
    now: i64,
) -> Result<(), GitHubError> {
    let marker = marker_for(ctx, Action::Cede, &item.id, "", now);
    issues::comment(
        &ctx.client,
        &ctx.repo,
        number,
        &format!(
            "`{}` is ceding this — another instance holds an earlier claim.\n\n{}",
            ctx.config.instance_id,
            marker.render()
        ),
    )?;
    let _ = crate::claims::release(
        &ctx.ledger,
        &ctx.repo.to_string(),
        &format!("issue#{number}"),
        &ctx.config.instance_id,
    );
    if let Some(state_id) = items::state_id_for_group(conn, &ctx.project_id, "cancelled") {
        let _ = agentflare_backend::item::update_state(conn, &item.id, &state_id);
    }
    Ok(())
}

#[allow(dead_code)] // consumer arrives in a later bridge task
fn issue_hash(issue: &Issue) -> String {
    let labels: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
    content_hash(
        &issue.title,
        issue.body.as_deref().unwrap_or(""),
        &issue.state,
        &labels,
    )
}

/// Exports only when the local content hash differs from the last CONFIRMED
/// successful write. The stored hash advances only after the write returns
/// Ok, so a failed write simply retries next tick.
#[allow(dead_code)] // consumer arrives in a later bridge task
fn export_if_dirty(
    ctx: &Ctx,
    conn: &rusqlite::Connection,
    issue: &Issue,
    item: &agentflare_backend::item::Item,
    now: i64,
) -> Result<bool, GitHubError> {
    let labels: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
    let completed = item.completed_at.is_some();
    let local_state = if completed { "closed" } else { "open" };
    let hash = content_hash(&item.name, &item.description, local_state, &labels);

    if items::last_hash(item).as_deref() == Some(hash.as_str()) {
        return Ok(false);
    }

    let action = if completed {
        Action::Done
    } else {
        Action::Progress
    };
    let marker = marker_for(ctx, action, &item.id, &hash, now);
    let note = if completed {
        format!("Completed by `{}`.", ctx.config.instance_id)
    } else {
        format!("Progress from `{}`.", ctx.config.instance_id)
    };
    issues::comment(
        &ctx.client,
        &ctx.repo,
        issue.number,
        &format!("{note}\n\n{}", marker.render()),
    )?;

    if completed {
        issues::close(&ctx.client, &ctx.repo, issue.number)?;
        let _ = crate::claims::done(
            &ctx.ledger,
            &ctx.repo.to_string(),
            &format!("issue#{}", issue.number),
            &ctx.config.instance_id,
            now,
        );
    }

    // Only now, after confirmed success, record the hash.
    let _ = agentflare_backend::item::update(
        conn,
        &item.id,
        agentflare_backend::item::UpdateItem {
            metadata: Some(items::with_last_hash(item, &hash)),
            ..Default::default()
        },
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::bridge::marker::{Action, Marker};
    use crate::github::test_support::{MockResponse, MockServer};

    const NOW: i64 = 1_754_000_000;

    fn marker_body(action: Action, owner: &str, ts: i64) -> String {
        Marker {
            action,
            owner: owner.to_string(),
            item: "i".into(),
            ts,
            hash: "h".into(),
        }
        .render()
    }

    #[test]
    fn issue_body_with_marker_appends_when_absent() {
        let m = Marker {
            action: Action::Claim,
            owner: "a:1".into(),
            item: "i".into(),
            ts: NOW,
            hash: "h".into(),
        };
        let out = issue_body_with_marker("hello", &m);
        assert!(out.starts_with("hello"));
        assert_eq!(Marker::parse(&out).unwrap().owner, "a:1");
    }

    #[test]
    fn issue_body_with_marker_replaces_a_previous_marker_rather_than_stacking() {
        let old = Marker {
            action: Action::Claim,
            owner: "a:1".into(),
            item: "i".into(),
            ts: 1,
            hash: "h".into(),
        };
        let new = Marker {
            ts: 2,
            ..old.clone()
        };
        let once = issue_body_with_marker("body", &old);
        let twice = issue_body_with_marker(&once, &new);
        assert_eq!(twice.matches("agentflare:v1").count(), 1);
        assert_eq!(Marker::parse(&twice).unwrap().ts, 2);
    }

    #[test]
    fn capacity_zero_claims_nothing_and_makes_no_write_calls() {
        // One response: the queue listing. Any claim attempt would need more.
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"number":7,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"}]}]"#,
        )]);
        let ctx = test_ctx(&server, 0);
        let (conn, project_id) = test_db();
        let report = run_once(&ctx_with_project(ctx, project_id), &conn, NOW).unwrap();
        assert!(report.claimed.is_empty());
        let reqs = server.requests();
        assert!(
            reqs.iter().all(|r| r.method == "GET"),
            "no writes when at capacity"
        );
    }

    #[test]
    fn winning_the_race_creates_a_local_item_linked_to_the_issue() {
        let server = MockServer::start(vec![
            // 1. queue listing
            MockResponse::json(
                200,
                r#"[{"number":7,"html_url":"u","state":"open","title":"Do the thing","body":"","labels":[{"name":"agentflare"}]}]"#,
            ),
            // 2. existing comments on #7 — none, so it is unclaimed
            MockResponse::json(200, "[]"),
            // 3. POST our claim comment
            MockResponse::json(201, r#"{"id":100}"#),
            // 4. re-fetch comments — ours is the only claim
            MockResponse::json(
                200,
                &format!(
                    r#"[{{"id":100,"user":{{"login":"u"}},"body":{}}}]"#,
                    serde_json::to_string(&marker_body(Action::Claim, "me:1", NOW)).unwrap()
                ),
            ),
            // 5. apply the claimed: label
            MockResponse::json(200, "[]"),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert_eq!(report.claimed, vec![7]);
        let item = crate::github::bridge::items::find_by_issue(&conn, &project_id, 7).unwrap();
        assert_eq!(item.name, "Do the thing");
        let _ = server.requests();
    }

    #[test]
    fn winning_also_records_the_claim_in_the_local_ledger() {
        // Regression guard: the ledger is a DIFFERENT database from items, and
        // the `claims::*` calls are best-effort (`let _ = ...`). Passing the
        // wrong connection fails silently, so assert the row actually landed —
        // without this, Layer 1 could be dead while every other test passes.
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                r#"[{"number":7,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"}]}]"#,
            ),
            MockResponse::json(200, "[]"),
            MockResponse::json(201, r#"{"id":100}"#),
            MockResponse::json(
                200,
                &format!(
                    r#"[{{"id":100,"user":{{"login":"u"}},"body":{}}}]"#,
                    serde_json::to_string(&marker_body(Action::Claim, "me:1", NOW)).unwrap()
                ),
            ),
            MockResponse::json(200, "[]"),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id);
        run_once(&ctx, &conn, NOW).unwrap();

        let held = crate::claims::list(&ctx.ledger, None, true, NOW, 1800).unwrap();
        assert_eq!(held.len(), 1, "claim must be recorded in the ledger");
        assert_eq!(held[0].target, "issue#7");
        assert_eq!(held[0].owner, "me:1");
        let _ = server.requests();
    }

    #[test]
    fn losing_the_race_creates_no_local_item() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                r#"[{"number":7,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"}]}]"#,
            ),
            MockResponse::json(200, "[]"),
            MockResponse::json(201, r#"{"id":100}"#),
            // Re-fetch reveals an EARLIER claim by another instance that we
            // did not see on the first read — eventual consistency.
            MockResponse::json(
                200,
                &format!(
                    r#"[{{"id":50,"user":{{"login":"u"}},"body":{}}},{{"id":100,"user":{{"login":"u"}},"body":{}}}]"#,
                    serde_json::to_string(&marker_body(Action::Claim, "other:9", NOW)).unwrap(),
                    serde_json::to_string(&marker_body(Action::Claim, "me:1", NOW)).unwrap()
                ),
            ),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert!(report.claimed.is_empty(), "must not claim after losing");
        assert!(crate::github::bridge::items::find_by_issue(&conn, &project_id, 7).is_none());
        let _ = server.requests();
    }

    #[test]
    fn a_rate_limit_ends_the_tick_without_erroring_the_loop() {
        let server = MockServer::start(vec![
            MockResponse::json(403, r#"{"message":"limited"}"#)
                .with_header("x-ratelimit-remaining", "0"),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id);
        // A rate limit is a normal outcome, not a hard failure: the tick
        // returns an empty report so the daemon keeps looping.
        let report = run_once(&ctx, &conn, NOW).unwrap();
        assert!(report.claimed.is_empty());
        let _ = server.requests();
    }

    fn test_ctx(server: &MockServer, max_claims: usize) -> Ctx {
        Ctx {
            client: server.client(Some("tok")),
            repo: crate::github::RepoId {
                owner: "o".into(),
                repo: "r".into(),
            },
            config: crate::github::bridge::config::BridgeConfig::from_values(
                Some("1"),
                None,
                Some(&max_claims.to_string()),
                None,
                "me:1".to_string(),
            ),
            project_id: String::new(),
            ledger: test_ledger(),
        }
    }

    /// In-memory claim ledger. The real one is `crate::db::open()`
    /// (agentflare.db) — a DIFFERENT database from the backend db, so tests
    /// must migrate the `claims` table explicitly here.
    fn test_ledger() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::claims::migrate(&conn).unwrap();
        conn
    }

    fn ctx_with_project(mut ctx: Ctx, project_id: String) -> Ctx {
        ctx.project_id = project_id;
        ctx
    }

    fn test_db() -> (rusqlite::Connection, String) {
        // Task 5 already defined this as `pub(crate)` — one fixture, not two.
        crate::github::bridge::items::tests::tests_support_db()
    }
}
