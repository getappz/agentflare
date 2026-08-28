//! One poll cycle. Ordering matters: re-verify (and cede) BEFORE doing new
//! work, so a lost race is dropped as early as possible; claim LAST, so
//! headroom reflects everything already taken on this tick.

use crate::github::bridge::claim as claim_rules;
use crate::github::bridge::config::{BridgeConfig, CLAIMED_LABEL_PREFIX};
use crate::github::bridge::items;
use crate::github::bridge::marker::{Action, Marker, content_hash};
use crate::github::models::Issue;
use crate::github::{Client, GitHubError, RepoId, issues};

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
pub struct TickReport {
    pub claimed: Vec<u64>,
    pub ceded: Vec<u64>,
    pub exported: Vec<u64>,
    /// The soft error that ended this tick early, if one did. Soft errors
    /// are normal operating conditions (rate limit, auth) and must not stop
    /// the daemon — but `Forbidden` is a permanently dead bridge, and
    /// swallowing it entirely meant nothing ever said so. Surfaced here so
    /// the runner can report a CHANGE in state rather than log every tick.
    pub soft_error: Option<String>,
}

// NOTE: markers live only in COMMENTS, never in the issue body. An earlier
// `issue_body_with_marker` helper existed for a body-marker design that was
// abandoned — editing the body would fight with humans editing the same
// field, whereas a comment is unambiguously the bridge's own.

/// Rate limiting and auth failure are expected operating conditions, not bugs:
/// they end the tick quietly so the daemon keeps looping and retries later.
fn is_soft(err: &GitHubError) -> bool {
    matches!(
        err,
        GitHubError::RateLimited(_) | GitHubError::NoAuth(_) | GitHubError::Forbidden(_)
    )
}

fn comment_pairs(ctx: &Ctx, number: u64) -> Result<Vec<(u64, String)>, GitHubError> {
    Ok(issues::list_comments(&ctx.client, &ctx.repo, number, None)?
        .into_iter()
        .map(|c| (c.id, c.body))
        .collect())
}

fn marker_for(ctx: &Ctx, action: Action, item_id: &str, hash: &str, now: i64) -> Marker {
    Marker {
        action,
        owner: ctx.config.instance_id.clone(),
        item: item_id.to_string(),
        ts: now,
        hash: hash.to_string(),
    }
}

pub fn run_once(ctx: &Ctx, conn: &rusqlite::Connection, now: i64) -> Result<TickReport, String> {
    let mut report = TickReport::default();
    match run_inner(ctx, conn, now, &mut report) {
        Ok(()) => Ok(report),
        // Soft errors keep whatever the tick already accomplished.
        Err(e) if is_soft(&e) => {
            report.soft_error = Some(e.to_string());
            Ok(report)
        }
        Err(e) => Err(e.to_string()),
    }
}

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

    // 1. Re-verify claims we believe we hold; heartbeat the ones we still
    //    hold, cede any we actually lost.
    for issue in &queued {
        let Some(item) = items::find_by_issue(conn, &ctx.project_id, issue.number) else {
            continue;
        };
        // Already cancelled or completed locally: skip, or a ceded item gets
        // re-ceded (and re-spams the issue with a "ceding this" comment)
        // every tick, forever — nothing else claims it after we cede once.
        if !items::is_active(conn, &item) {
            continue;
        }
        let comments = comment_pairs(ctx, issue.number)?;
        let ours = claim_rules::resolve_holder(&comments, now, ctx.config.ttl_secs)
            .filter(|h| h.marker.owner == ctx.config.instance_id);
        if let Some(held) = ours {
            heartbeat(ctx, &comments, issue.number, &held, &item, now)?;
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
        // A ceded item is still LINKED to its issue, so without this it
        // reaches `export_if_dirty` and posts "Progress from …" on an issue
        // another instance now owns. And it will: labels feed the hash, and
        // the winner adds its own `claimed:` label, so the hash drifts by
        // construction. Deliberately `is_ceded`, not `!is_active` —
        // a completed item still owes this issue its `done` export.
        if items::is_ceded(conn, &item) {
            continue;
        }
        if export_if_dirty(ctx, conn, issue, &item, now)? {
            report.exported.push(issue.number);
        }
    }

    // 3. Claim new work, up to remaining headroom.
    //
    // `remaining` counts SUCCESSES, not attempts. Bounding the loop itself
    // (`take(headroom)`) meant a queue whose first few entries were already
    // tracked or held by someone else consumed the whole allowance and this
    // instance claimed nothing — every tick, for as long as the queue head
    // stayed put, while free issues sat just below it.
    let held = held_count(conn, ctx);
    let mut remaining = ctx.config.max_claims.saturating_sub(held);
    for issue in &queued {
        if remaining == 0 {
            break;
        }
        // Ceded THIS tick: step 1 just established a rival holds it, so
        // re-probing now would only re-read comments we already read and
        // re-learn the answer. Wait for the next tick.
        if report.ceded.contains(&issue.number) {
            continue;
        }
        // Otherwise a linked item's mere existence is NOT a reason to skip:
        // an item ceded on an EARLIER tick is `cancelled`, and treating that
        // as "already tracked" made every cede permanent — the issue could
        // never be picked up by this instance again, even once the contention
        // that caused the cede was long gone. Ceded items are re-adoptable;
        // active and completed ones are not (a completed one still owes its
        // issue a `done` export).
        if items::find_by_issue(conn, &ctx.project_id, issue.number)
            .is_some_and(|item| !items::is_ceded(conn, &item))
        {
            continue;
        }
        if try_claim(ctx, conn, issue, now)? {
            report.claimed.push(issue.number);
            remaining -= 1;
        }
    }
    Ok(())
}

/// How stale our own marker may get before the heartbeat refreshes it.
///
/// Half the TTL, so one lost or failed refresh still leaves a full half-TTL
/// of ticks to recover in before `resolve_holder` would expire us.
fn heartbeat_due_after(ttl_secs: i64) -> i64 {
    (ttl_secs / 2).max(1)
}

/// Keeps a claim we still hold alive, on both layers.
///
/// Without this the bridge cedes its OWN live work on a wall-clock timer:
/// `resolve_holder` expires an owner whose newest marker is older than the
/// TTL, and before this existed the only thing that ever wrote a marker was
/// `export_if_dirty` — which fires solely on a content-hash change. An issue
/// being *worked* rather than *edited* therefore went quiet, aged past the
/// TTL, and got ceded to nobody.
///
/// The remote refresh EDITS the comment that already carries our claim
/// marker rather than posting a new one: same comment id, so
/// `resolve_holder`'s lowest-id-wins ordering and `is_ceded`'s id comparison
/// are both untouched, and the issue does not accumulate a bookkeeping
/// comment every half-TTL. It also rewrites `item=` with the real item id,
/// which is not known yet when the claim is first posted.
fn heartbeat(
    ctx: &Ctx,
    comments: &[(u64, String)],
    number: u64,
    held: &claim_rules::Holder,
    item: &agentflare_backend::item::Item,
    now: i64,
) -> Result<bool, GitHubError> {
    // The local ledger is cheap and purely local — refresh it every tick we
    // hold, regardless of whether the remote marker is due.
    if let Err(e) = crate::claims::heartbeat(
        &ctx.ledger,
        &ctx.repo.to_string(),
        &format!("issue#{number}"),
        &ctx.config.instance_id,
        now,
    ) {
        eprintln!("github bridge: ledger heartbeat failed: {e}");
    }

    // Liveness is judged from our NEWEST marker of any action, so a recent
    // export already covers us and no write is needed.
    let due = claim_rules::owner_liveness(comments, &ctx.config.instance_id)
        .is_none_or(|ts| now.saturating_sub(ts) >= heartbeat_due_after(ctx.config.ttl_secs));
    if !due {
        return Ok(false);
    }

    let refreshed = Marker {
        action: Action::Claim,
        owner: ctx.config.instance_id.clone(),
        item: item.id.clone(),
        ts: now,
        hash: held.marker.hash.clone(),
    };
    issues::update_comment(
        &ctx.client,
        &ctx.repo,
        held.comment_id,
        &format!(
            "Claiming this for `{}`.\n\n{}",
            ctx.config.instance_id,
            refreshed.render()
        ),
    )?;
    Ok(true)
}

fn held_count(conn: &rusqlite::Connection, ctx: &Ctx) -> usize {
    items_tracked(conn, ctx).len()
}

fn items_tracked(conn: &rusqlite::Connection, ctx: &Ctx) -> Vec<agentflare_backend::item::Item> {
    agentflare_backend::item::list_by_project(conn, &ctx.project_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.external_source.as_deref() == Some(items::EXTERNAL_SOURCE))
        // Not `completed_at.is_none()`: cancelling never sets `completed_at`
        // (see `items::is_active`), so that alone would count ceded items
        // toward capacity forever. Filter on state group instead.
        .filter(|i| items::is_active(conn, i))
        .collect()
}

/// Whether an issue's `author_association` is trusted enough to let its raw
/// title/body become a work item's description -- that description later
/// becomes an autonomous agent's literal prompt (see `build_prompt` in
/// `cli/work.rs`), run with a permission-bypass flag. See
/// `github::is_trusted_author_association` for the trust bar itself.
fn is_trusted_author(issue: &Issue) -> bool {
    crate::github::is_trusted_author_association(&issue.author_association)
}

/// Optimistic two-step claim: post our marker, then re-read and check we are
/// the earliest. GitHub offers no compare-and-swap on labels or comments, so
/// this is the closest available approximation.
fn try_claim(
    ctx: &Ctx,
    conn: &rusqlite::Connection,
    issue: &Issue,
    now: i64,
) -> Result<bool, GitHubError> {
    // Reject before doing ANYTHING else -- not even reading comments -- so
    // an untrusted issue gets zero engagement from the bridge, not just a
    // skipped claim.
    if !is_trusted_author(issue) {
        return Ok(false);
    }
    let before = comment_pairs(ctx, issue.number)?;
    match claim_rules::resolve_holder(&before, now, ctx.config.ttl_secs) {
        // GitHub says WE hold it, but we got here — so we have no active
        // local item for it. Adopt rather than bail: bailing meant sitting
        // out a full TTL waiting for our OWN marker to expire. Reachable
        // from a `Transport` blip between posting the claim and re-reading
        // comments, from `state_id_for_group` returning None after we won,
        // and from local db loss — which `marker.rs` advertises as
        // recoverable precisely because the marker carries enough to rebuild.
        Some(h) if h.marker.owner == ctx.config.instance_id => {
            return record_claim(ctx, conn, issue, Some(h.comment_id), now);
        }
        Some(_) => return Ok(false), // held by someone else, live
        None => {}
    }

    let hash = issue_hash(issue);
    // The item does not exist yet — it is only created once we know we won
    // the race — so the marker cannot name it. `record_claim` rewrites this
    // comment with the real id rather than leaving `item=pending` on the
    // issue for the rest of the claim's life.
    let marker = marker_for(ctx, Action::Claim, "pending", &hash, now);
    let comment_id = issues::comment(
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

    record_claim(ctx, conn, issue, Some(comment_id), now)
}

/// Everything that follows winning an issue: a local item in the `started`
/// group, a ledger row, the `claimed:` label, and — given
/// `claim_comment_id` — a rewrite of the claim marker to name the item.
/// Shared by the ordinary claim path and by the adopt-our-own-orphan path,
/// which must land exactly the same local state.
fn record_claim(
    ctx: &Ctx,
    conn: &rusqlite::Connection,
    issue: &Issue,
    claim_comment_id: Option<u64>,
    now: i64,
) -> Result<bool, GitHubError> {
    let Some(state_id) = items::state_id_for_group(conn, &ctx.project_id, "started") else {
        // The claim comment is already posted by this point, so bailing
        // silently leaves our marker on the issue with no local item behind
        // it: step 1 skips the issue (nothing to find), so nothing
        // heartbeats it and nothing cedes it, and it stays blocked for a
        // full TTL with no explanation. `cede` logs the mirror of this.
        eprintln!(
            "github bridge: project has no `started` state; cannot record the claim on #{}",
            issue.number
        );
        return Ok(false);
    };
    // Re-adopt a previously ceded item rather than creating a second one:
    // `find_by_issue` returns the FIRST match, so a duplicate would leave the
    // bridge permanently updating one row while reading another.
    let created = match items::find_by_issue(conn, &ctx.project_id, issue.number) {
        Some(existing) => {
            agentflare_backend::item::update_state(conn, &existing.id, &state_id)
                .map_err(|e| GitHubError::Parse(e.to_string()))?;
            existing
        }
        None => {
            // Labeling `ready-for-work` (when a work agent is configured
            // and the project has that label at all -- skipped otherwise
            // rather than creating it out of nowhere, same reasoning as
            // `handoff_impl`) is what actually gets this claimed issue
            // dispatched: `spawn_supervisor_discovery`'s own tick is what
            // launches the agent from there, not this function. Without a
            // work agent, `assignee_agent` stays the bridge's own instance
            // id -- claimed but nothing picks it up, same as before this
            // existed.
            let ready_label_id = ctx.config.work_agent.as_ref().and_then(|_| {
                agentflare_backend::label::list_by_project(conn, &ctx.project_id)
                    .ok()
                    .and_then(|labels| {
                        labels
                            .into_iter()
                            .find(|l| l.name == crate::supervisor::READY_LABEL)
                    })
                    .map(|l| l.id)
            });
            // This decision is made once, right here, not re-evaluated on
            // later ticks for this same issue (see the `Some(existing)` arm
            // above) -- so a `work_agent` configured after this point never
            // retroactively dispatches it. Silence here reads as "being
            // worked" when it's actually just claimed and stuck; say so.
            if ctx.config.work_agent.is_none() {
                eprintln!(
                    "github bridge: claimed #{} with no work_agent configured — it will \
                     not be auto-dispatched; set AGENTFLARE_BRIDGE_WORK_AGENT or dispatch \
                     it manually (`agentflare work`)",
                    issue.number
                );
            }
            // `handoff`'s `recipient="github"` path embeds the full
            // structured payload (content/completed/remaining/thread_id) as
            // a hidden marker after the human-readable body -- recover it
            // so a bridge-originated item carries the same fields a local
            // handoff would, instead of only the rendered issue body with no
            // metadata. An issue opened by hand (or from before this
            // existed) has no marker; `handoff` returns `None` and this
            // falls back to the old behavior unchanged.
            let handoff = issue
                .body
                .as_deref()
                .and_then(crate::github::bridge::handoff_payload::HandoffPayload::extract);
            let description = handoff
                .as_ref()
                .map(|p| p.content.clone())
                .or_else(|| issue.body.clone());
            let metadata = handoff.as_ref().map(|p| {
                let mut m = serde_json::json!({
                    "completed": p.completed,
                    "remaining": p.remaining,
                });
                if let Some(t) = &p.thread_id {
                    m["thread_id"] = serde_json::json!(t);
                }
                m.to_string()
            });

            agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: ctx.project_id.clone(),
                    state_id,
                    name: issue.title.clone(),
                    description,
                    priority: None,
                    parent_id: None,
                    assignee_agent: Some(
                        ctx.config
                            .work_agent
                            .clone()
                            .unwrap_or_else(|| ctx.config.instance_id.clone()),
                    ),
                    sort_order: None,
                    external_source: Some(items::EXTERNAL_SOURCE.to_string()),
                    external_id: Some(issue.number.to_string()),
                    metadata,
                    label_ids: ready_label_id.into_iter().collect(),
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .map_err(|e| GitHubError::Parse(e.to_string()))?
        }
    };

    // Local ledger too, so this instance's OWN agents do not double-claim.
    // NOTE: `&ctx.ledger`, NOT `conn` — separate databases.
    if let Err(e) = crate::claims::acquire(
        &ctx.ledger,
        &ctx.repo.to_string(),
        &format!("issue#{}", issue.number),
        &ctx.config.instance_id,
        None,
        None,
        now,
        ctx.config.ttl_secs,
    ) {
        eprintln!(
            "github bridge: ledger acquire failed for #{}: {e}",
            issue.number
        );
    }

    // Name the item in the marker now that it exists. Best-effort: the claim
    // is already valid without it, and the next heartbeat rewrites the same
    // comment anyway.
    if let Some(comment_id) = claim_comment_id
        && let Err(e) = issues::update_comment(
            &ctx.client,
            &ctx.repo,
            comment_id,
            &format!(
                "Claiming this for `{}`.\n\n{}",
                ctx.config.instance_id,
                marker_for(ctx, Action::Claim, &created.id, &issue_hash(issue), now).render()
            ),
        )
    {
        eprintln!(
            "github bridge: could not name the item in #{}'s claim marker: {e}",
            issue.number
        );
    }

    // Seed the export latch with what we just imported. Without this the item
    // has no stored hash, so the very NEXT tick sees "changed" and posts a
    // "Progress from …" comment that reports nothing — the item's name and
    // description were copied from the issue moments earlier. Live-verified:
    // every claim produced a redundant second comment. A real local edit
    // still changes the hash and still exports.
    let imported = export_hash(&created, issue);
    if let Err(e) = store_hash(conn, &created.id, Some(&imported)) {
        // Not fatal: the cost is one redundant progress comment next tick.
        eprintln!(
            "github bridge: could not seed the export hash for #{}: {e}",
            issue.number
        );
    }

    issues::add_labels(
        &ctx.client,
        &ctx.repo,
        issue.number,
        &[format!("{CLAIMED_LABEL_PREFIX}{}", ctx.config.instance_id)],
    )?;
    if let Err(e) = issues::add_labels(
        &ctx.client,
        &ctx.repo,
        issue.number,
        &[format!(
            "beacon:{}",
            crate::github::bridge::config::machine_label()
        )],
    ) {
        eprintln!(
            "github bridge: could not add beacon label to #{}: {e}",
            issue.number
        );
    }
    Ok(true)
}

fn cede(
    ctx: &Ctx,
    conn: &rusqlite::Connection,
    number: u64,
    item: &agentflare_backend::item::Item,
    now: i64,
) -> Result<(), GitHubError> {
    // Cancel LOCALLY FIRST, and only announce once that stuck. This
    // ordering is the idempotence latch: step 1 decides whether to cede from
    // the item's state, so announcing first and then failing to persist the
    // cancellation re-cedes the same issue every `interval_secs` — 1440
    // identical comments a day at the defaults. `database is locked` from
    // the dashboard or MCP server writing the same file makes that reachable.
    let Some(cancelled) = items::state_id_for_group(conn, &ctx.project_id, "cancelled") else {
        eprintln!("github bridge: no cancelled state in project; not ceding #{number}");
        return Ok(());
    };
    if let Err(e) = agentflare_backend::item::update_state(conn, &item.id, &cancelled) {
        eprintln!("github bridge: could not cancel item for #{number} ({e}); not ceding");
        return Ok(());
    }

    let marker = marker_for(ctx, Action::Cede, &item.id, "", now);
    let posted = issues::comment(
        &ctx.client,
        &ctx.repo,
        number,
        &format!(
            "`{}` is ceding this — another instance holds an earlier claim.\n\n{}",
            ctx.config.instance_id,
            marker.render()
        ),
    )
    .map(|_| ());
    if let Err(e) = posted {
        // Put the item back so the next tick retries the whole cede rather
        // than leaving it cancelled with no cede marker on the issue — which
        // would read to every other instance as "still claimed by us".
        if let Err(revert) = agentflare_backend::item::update_state(conn, &item.id, &item.state_id)
        {
            eprintln!("github bridge: could not restore item after a failed cede: {revert}");
        }
        return Err(e);
    }

    if let Err(e) = crate::claims::release(
        &ctx.ledger,
        &ctx.repo.to_string(),
        &format!("issue#{number}"),
        &ctx.config.instance_id,
    ) {
        eprintln!("github bridge: ledger release failed for #{number}: {e}");
    }
    drop_claimed_label(ctx, number);
    Ok(())
}

/// Takes this instance's `claimed:` label back off an issue it no longer
/// holds. Best-effort — the marker, not the label, is what the protocol
/// decides on — but without it every issue accumulates a `claimed:` label
/// per instance that ever touched it, and a human reading the issue list
/// sees work claimed by nobody.
fn drop_claimed_label(ctx: &Ctx, number: u64) {
    let label = format!("{CLAIMED_LABEL_PREFIX}{}", ctx.config.instance_id);
    if let Err(e) = issues::remove_label(&ctx.client, &ctx.repo, number, &label) {
        eprintln!("github bridge: could not remove {label} from #{number}: {e}");
    }
}

/// The issue's labels MINUS the bridge's own `claimed:` bookkeeping.
///
/// The hash exists to notice that an issue's CONTENT drifted. A `claimed:`
/// label is not content — it is something the bridge itself just wrote. Left
/// in, the sequence is: we claim, we add `claimed:<us>`, and on the very next
/// tick the hash has "changed", so we post a "Progress from …" comment
/// reporting our own side effect. That is one junk comment on every issue the
/// bridge ever claims, and it was live-verified against a real repo before
/// this filter existed.
fn content_labels(issue: &Issue) -> Vec<String> {
    issue
        .labels
        .iter()
        .map(|l| l.name.clone())
        .filter(|n| !n.starts_with(CLAIMED_LABEL_PREFIX))
        .collect()
}

fn issue_hash(issue: &Issue) -> String {
    content_hash(
        &issue.title,
        issue.body.as_deref().unwrap_or(""),
        &issue.state,
        &content_labels(issue),
    )
}

/// The export latch's value for `item` against `issue`.
///
/// One definition, because two callers depend on agreeing exactly:
/// `export_if_dirty` compares against it, and `record_claim` seeds it. If
/// they computed it differently, every freshly claimed issue would get a
/// spurious progress comment on its next tick — which is the bug that made
/// this a shared function.
fn export_hash(item: &agentflare_backend::item::Item, issue: &Issue) -> String {
    let local_state = if item.completed_at.is_some() {
        "closed"
    } else {
        "open"
    };
    content_hash(
        &item.name,
        &item.description,
        local_state,
        &content_labels(issue),
    )
}

/// Exports only when the local content hash differs from the last stored one.
///
/// The stored hash is the idempotence latch, so it is written BEFORE the
/// GitHub call and the GitHub call is gated on it: a comment must never be
/// posted that we could then fail to record, or the identical comment
/// reposts every `interval_secs` forever. If the remote write then fails the
/// latch is rolled back, so the export retries next tick rather than being
/// silently skipped.
fn export_if_dirty(
    ctx: &Ctx,
    conn: &rusqlite::Connection,
    issue: &Issue,
    item: &agentflare_backend::item::Item,
    now: i64,
) -> Result<bool, GitHubError> {
    let completed = item.completed_at.is_some();
    let hash = export_hash(item, issue);

    if items::last_hash(item).as_deref() == Some(hash.as_str()) {
        // Nothing new to say — but the close is tracked separately from the
        // latch (see below), so a completed item whose issue is still open
        // has one outstanding from an earlier tick.
        if completed {
            close_if_still_open(ctx, issue);
        }
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

    // Latch first — no GitHub write without a persisted record of it.
    if let Err(e) = store_hash(conn, &item.id, Some(&hash)) {
        eprintln!(
            "github bridge: could not record the export hash for #{} ({e}); \
             skipping the export rather than risk re-posting it every tick",
            issue.number
        );
        return Ok(false);
    }

    let posted = issues::comment(
        &ctx.client,
        &ctx.repo,
        issue.number,
        &format!("{note}\n\n{}", marker.render()),
    );

    if let Err(e) = posted {
        // Roll the latch back so this export is retried instead of being
        // suppressed until the content happens to change. Clearing the key
        // rather than restoring the tick's stale snapshot: the previous
        // value was either absent or a hash we are re-deriving anyway, and
        // restoring the snapshot wholesale would revert anyone else's
        // metadata written since.
        let previous = items::last_hash(item);
        if let Err(revert) = store_hash(conn, &item.id, previous.as_deref()) {
            eprintln!("github bridge: could not roll back the export hash: {revert}");
        }
        return Err(e);
    }

    // The comment landed, so the latch stays put even if the close fails.
    // Sharing one latch between the two meant a failed close rolled back a
    // comment that HAD been posted, and the next tick — seeing the same
    // content hash as dirty again — posted a second "Completed by …". Every
    // failed close added another duplicate, and close is the call most
    // likely to fail on its own (a locked or already-modified issue).
    if completed {
        close_if_still_open(ctx, issue);
    }

    if completed
        && let Err(e) = crate::claims::done(
            &ctx.ledger,
            &ctx.repo.to_string(),
            &format!("issue#{}", issue.number),
            &ctx.config.instance_id,
            now,
        )
    {
        eprintln!(
            "github bridge: ledger done failed for #{}: {e}",
            issue.number
        );
    }
    if completed {
        // The work is finished, so the claim is over — take our label back
        // off before the issue closes.
        drop_claimed_label(ctx, issue.number);
    }
    Ok(true)
}

/// Closes the issue behind a completed item, if it is not closed already.
///
/// Deliberately best-effort and NOT tied to the export latch: the `done`
/// comment may already be recorded when the close fails, and rolling the
/// latch back for that would repost the comment. A still-open issue is its
/// own retry signal — it stays in the queue until the close succeeds.
fn close_if_still_open(ctx: &Ctx, issue: &Issue) {
    if issue.state == "closed" {
        return;
    }
    if let Err(e) = issues::close(&ctx.client, &ctx.repo, issue.number) {
        eprintln!(
            "github bridge: could not close #{} ({e}); will retry next tick",
            issue.number
        );
    }
}

/// Writes `github_last_hash` onto an item, merging into the item's CURRENT
/// metadata rather than the snapshot the tick started with.
///
/// `metadata` is one JSON column and `item::update` replaces it wholesale.
/// The tick's snapshot is read at the top of `run_inner` and written after a
/// GitHub round trip, so it is seconds stale — and an `item(update)` through
/// the MCP server in that window would be silently reverted. Re-reading
/// immediately before the write narrows that to the width of a single
/// statement.
///
/// `None` clears the key, which is what the rollback path wants.
fn store_hash(
    conn: &rusqlite::Connection,
    item_id: &str,
    hash: Option<&str>,
) -> Result<(), agentflare_backend::Error> {
    let current = agentflare_backend::item::get(conn, item_id)?;
    let metadata = match hash {
        Some(h) => items::with_last_hash(&current, h),
        None => items::without_last_hash(&current),
    };
    agentflare_backend::item::update(
        conn,
        item_id,
        agentflare_backend::item::UpdateItem {
            metadata: Some(metadata),
            ..Default::default()
        },
    )
    .map(|_| ())
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
    fn capacity_zero_claims_nothing_and_makes_no_write_calls() {
        // One response: the queue listing. Any claim attempt would need more.
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"number":7,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"}],"author_association":"OWNER"}]"#,
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
                r#"[{"number":7,"html_url":"u","state":"open","title":"Do the thing","body":"","labels":[{"name":"agentflare"}],"author_association":"OWNER"}]"#,
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
            // 5. rewrite the marker now the item id is known
            MockResponse::json(200, r#"{"id":100}"#),
            // 6. apply the claimed: label
            MockResponse::json(200, "[]"),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert_eq!(report.claimed, vec![7]);
        let item = crate::github::bridge::items::find_by_issue(&conn, &project_id, 7).unwrap();
        assert_eq!(item.name, "Do the thing");

        // The claim marker is posted before the item exists, so it goes out
        // as `item=pending`. It must not STAY that way for the life of the
        // claim — the rewrite names the item the moment there is one.
        let reqs = server.requests();
        let rewrite = reqs
            .iter()
            .find(|r| r.method == "PATCH")
            .expect("the claim marker must be amended with the item id");
        let sent: serde_json::Value = serde_json::from_str(&rewrite.body).unwrap();
        let marker = Marker::parse(sent["body"].as_str().unwrap()).unwrap();
        assert_eq!(marker.item, item.id);

        // Without a configured work agent, a claim is visible but nobody's
        // been told to work it -- assignee stays the bridge's own instance
        // id, not a real dispatchable agent name.
        assert_eq!(item.assignee_agent.as_deref(), Some("me:1"));
    }

    #[test]
    fn winning_a_claim_also_adds_the_beacon_label() {
        crate::paths::test_support::with_temp_home(|| {
            let server = MockServer::start(vec![
                MockResponse::json(
                    200,
                    r#"[{"number":7,"html_url":"u","state":"open","title":"Do the thing","body":"","labels":[{"name":"agentflare"}],"author_association":"OWNER"}]"#,
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
                MockResponse::json(200, r#"{"id":100}"#),
                MockResponse::json(200, "[]"), // claimed: label
                MockResponse::json(200, "[]"), // beacon: label
            ]);
            let (conn, project_id) = test_db();
            let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
            let report = run_once(&ctx, &conn, NOW).unwrap();

            assert_eq!(report.claimed, vec![7]);
            let reqs = server.requests();
            let beacon_call = reqs
                .iter()
                .filter(|r| r.method == "POST" && r.path.ends_with("/issues/7/labels"))
                .nth(1)
                .expect("expected a second labels POST for the beacon label");
            assert!(
                beacon_call.body.contains("beacon:"),
                "expected a beacon:<machine> label, got: {}",
                beacon_call.body
            );
        });
    }

    #[test]
    fn winning_a_claim_with_a_work_agent_configured_dispatches_it() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                r#"[{"number":7,"html_url":"u","state":"open","title":"Do the thing","body":"","labels":[{"name":"agentflare"}],"author_association":"OWNER"}]"#,
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
            MockResponse::json(200, r#"{"id":100}"#),
            MockResponse::json(200, "[]"),
        ]);
        let (conn, project_id) = test_db();
        let project = agentflare_backend::project::get(&conn, &project_id).unwrap();
        agentflare_backend::label::create(
            &conn,
            agentflare_backend::label::CreateLabel {
                project_id: Some(project_id.clone()),
                workspace_id: project.workspace_id,
                name: crate::supervisor::READY_LABEL.to_string(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();

        let mut ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        ctx.config.work_agent = Some("claude-code".to_string());
        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert_eq!(report.claimed, vec![7]);
        let item = crate::github::bridge::items::find_by_issue(&conn, &project_id, 7).unwrap();
        assert_eq!(
            item.assignee_agent.as_deref(),
            Some("claude-code"),
            "assignee must be the configured work agent, not the bridge's own instance id"
        );
        let label_ids = agentflare_backend::item::list_labels(&conn, &item.id).unwrap();
        let label_names: Vec<String> = label_ids
            .iter()
            .map(|id| agentflare_backend::label::get(&conn, id).unwrap().name)
            .collect();
        assert!(
            label_names.contains(&crate::supervisor::READY_LABEL.to_string()),
            "expected ready-for-work label, got {label_names:?}"
        );
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
                r#"[{"number":7,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"}],"author_association":"OWNER"}]"#,
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
            MockResponse::json(200, r#"{"id":100}"#), // marker rewrite
            MockResponse::json(200, "[]"),            // claimed: label
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
    fn claiming_an_issue_published_by_handoff_recovers_its_full_payload() {
        // `handoff`'s recipient="github" path embeds content/completed/
        // remaining/thread_id as a hidden marker after the visible body
        // (`handoff_payload::HandoffPayload`); a bare `issue.body.clone()`
        // would only ever see the visible half.
        let payload = crate::github::bridge::handoff_payload::HandoffPayload {
            key: "thread-1".into(),
            content: "the full content".into(),
            completed: "done so far".into(),
            remaining: "left to do".into(),
            thread_id: Some("thread-1".into()),
        };
        let body = payload.embed("visible description");
        let issue_list = format!(
            r#"[{{"number":7,"html_url":"u","state":"open","title":"t","body":{},"labels":[{{"name":"agentflare"}}],"author_association":"OWNER"}}]"#,
            serde_json::to_string(&body).unwrap()
        );
        let server = MockServer::start(vec![
            MockResponse::json(200, &issue_list),
            MockResponse::json(200, "[]"),
            MockResponse::json(201, r#"{"id":100}"#),
            MockResponse::json(
                200,
                &format!(
                    r#"[{{"id":100,"user":{{"login":"u"}},"body":{}}}]"#,
                    serde_json::to_string(&marker_body(Action::Claim, "me:1", NOW)).unwrap()
                ),
            ),
            MockResponse::json(200, r#"{"id":100}"#), // marker rewrite
            MockResponse::json(200, "[]"),            // claimed: label
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert_eq!(report.claimed, vec![7]);
        let item = crate::github::bridge::items::find_by_issue(&conn, &project_id, 7).unwrap();
        assert_eq!(
            item.description, "the full content",
            "description must come from the embedded payload's content, not the visible body"
        );
        let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
        assert_eq!(metadata["completed"], "done so far");
        assert_eq!(metadata["remaining"], "left to do");
        assert_eq!(metadata["thread_id"], "thread-1");
    }

    #[test]
    fn losing_the_race_creates_no_local_item() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                r#"[{"number":7,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"}],"author_association":"OWNER"}]"#,
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
    fn an_untrusted_authors_issue_is_never_claimed_or_engaged() {
        // Anyone who can get an issue opened (and, on a repo with lax
        // triage permissions, labeled) must NOT be able to get its raw
        // body turned into a work item, an autonomous agent prompt, and
        // eventually a `--dangerously-skip-permissions` shell -- that is
        // an indirect-prompt-injection-to-RCE path. Only one response is
        // queued (the listing) because a rejected issue must not even get
        // as far as reading its comments or posting a claim -- any write
        // call at all would be a bug here.
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"number":7,"html_url":"u","state":"open","title":"t","body":"do something","labels":[{"name":"agentflare"}],"author_association":"CONTRIBUTOR"}]"#,
        )]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert!(report.claimed.is_empty());
        assert!(crate::github::bridge::items::find_by_issue(&conn, &project_id, 7).is_none());
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1, "must not read comments or write anything");
        assert_eq!(reqs[0].method, "GET");
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

    #[test]
    fn second_tick_after_a_cede_does_not_re_cede() {
        // Regression guard for the "ceded items are re-ceded forever" bug:
        // step 1 must skip items already moved to the `cancelled` group, or
        // the second tick posts a duplicate "is ceding this" comment.
        //
        // Tick 2 DOES re-probe the issue — a cede is recoverable, not
        // terminal — so it reads the comments again; what it must not do is
        // cede a second time.
        let rival = marker_body(Action::Claim, "other:9", NOW);
        let server = MockServer::start(vec![
            // tick 1: queue listing
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            // tick 1: comments on #7 — nobody holds it live, so we cede
            MockResponse::json(200, "[]"),
            // tick 1: cede's comment post
            MockResponse::json(201, r#"{"id":200}"#),
            // tick 1: and the claimed: label comes back off
            MockResponse::json(200, "[]"),
            // tick 2: queue listing
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            // tick 2: the reclaim probe finds a live rival, so it stops there
            MockResponse::json(200, &one_comment(50, &rival)),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        seed_synced_item(&conn, &project_id, 7, "started");

        let first = run_once(&ctx, &conn, NOW).unwrap();
        assert_eq!(first.ceded, vec![7], "first tick must cede the lost claim");

        let second = run_once(&ctx, &conn, NOW).unwrap();
        assert!(
            second.ceded.is_empty(),
            "a second tick must not re-cede an already-cancelled item"
        );
        assert!(
            second.claimed.is_empty(),
            "and must not take an issue a live rival holds"
        );
        let posts = server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST")
            .count();
        assert_eq!(posts, 1, "exactly one cede comment across both ticks");
    }

    #[test]
    fn a_ceded_item_does_not_count_toward_held_capacity() {
        // Regression guard for the "cancelled items permanently consume
        // capacity" bug: `held_count` must exclude `cancelled`-group items,
        // not merely filter on `completed_at` (cancelling never sets it).
        let server = MockServer::start(vec![
            MockResponse::json(201, r#"{"id":1}"#), // cede's comment post
            MockResponse::json(200, "[]"),          // the claimed: label removal
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());

        let state = items::state_id_for_group(&conn, &project_id, "started").unwrap();
        let item = agentflare_backend::item::create(
            &conn,
            agentflare_backend::item::CreateItem {
                project_id: project_id.clone(),
                state_id: state,
                name: "t".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: Some(items::EXTERNAL_SOURCE.to_string()),
                external_id: Some("7".into()),
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap();

        cede(&ctx, &conn, 7, &item, NOW).unwrap();

        assert_eq!(
            held_count(&conn, &ctx),
            0,
            "a ceded item must not consume claim capacity"
        );
        let _ = server.requests();
    }

    #[test]
    fn a_completed_item_does_not_count_toward_held_capacity() {
        let server = MockServer::start(vec![]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());

        let state = items::state_id_for_group(&conn, &project_id, "completed").unwrap();
        agentflare_backend::item::create(
            &conn,
            agentflare_backend::item::CreateItem {
                project_id: project_id.clone(),
                state_id: state,
                name: "t".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: Some(items::EXTERNAL_SOURCE.to_string()),
                external_id: Some("7".into()),
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap();

        assert_eq!(
            held_count(&conn, &ctx),
            0,
            "a completed item must not consume claim capacity"
        );
    }

    /// A locally-tracked item linked to `number`, in `group`, whose export
    /// hash is already in sync with `issue_json`'s content — so the export
    /// step is a guaranteed no-op and consumes no mock response. Tests that
    /// want to focus on one step of the tick can then queue exactly the
    /// responses that step needs.
    fn seed_synced_item(
        conn: &rusqlite::Connection,
        project_id: &str,
        number: u64,
        group: &str,
    ) -> agentflare_backend::item::Item {
        let state = items::state_id_for_group(conn, project_id, group).unwrap();
        let hash = content_hash("t", "", "open", &["agentflare".to_string()]);
        let metadata = format!(
            r#"{{"github_last_hash":{}}}"#,
            serde_json::to_string(&hash).unwrap()
        );
        agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project_id.to_string(),
                state_id: state,
                name: "t".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: Some("me:1".into()),
                sort_order: None,
                external_source: Some(items::EXTERNAL_SOURCE.to_string()),
                external_id: Some(number.to_string()),
                metadata: Some(metadata),
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap()
    }

    const QUEUE_ONE_ISSUE: &str = r#"[{"number":7,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"}],"author_association":"OWNER"}]"#;

    fn one_comment(id: u64, body: &str) -> String {
        format!(
            r#"[{{"id":{id},"user":{{"login":"u"}},"body":{}}}]"#,
            serde_json::to_string(body).unwrap()
        )
    }

    #[test]
    fn an_idle_holder_refreshes_its_marker_instead_of_ceding_its_own_work() {
        // THE regression guard for the self-cede treadmill. Before the
        // heartbeat existed, the only thing that ever wrote a marker was
        // `export_if_dirty` — which fires on a content-hash change. An issue
        // being worked rather than edited went quiet, aged past the TTL, and
        // the instance ceded its own live work to nobody, cancelled the item,
        // and could never reclaim it.
        let ttl = crate::claims::ttl_secs();
        let stale = NOW - (ttl / 2) - 1;
        let server = MockServer::start(vec![
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            MockResponse::json(
                200,
                &one_comment(100, &marker_body(Action::Claim, "me:1", stale)),
            ),
            MockResponse::json(200, r#"{"id":100}"#), // the heartbeat PATCH
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        seed_synced_item(&conn, &project_id, 7, "started");

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert!(
            report.ceded.is_empty(),
            "an instance that still holds the issue must never cede to itself"
        );
        let reqs = server.requests();
        let patch = reqs
            .iter()
            .find(|r| r.method == "PATCH")
            .expect("a stale marker must be refreshed");
        assert_eq!(
            patch.path, "/repos/o/r/issues/comments/100",
            "the refresh must EDIT the existing claim comment, not post a new one"
        );
        let sent: serde_json::Value = serde_json::from_str(&patch.body).unwrap();
        let refreshed = Marker::parse(sent["body"].as_str().unwrap()).unwrap();
        assert_eq!(refreshed.ts, NOW, "the marker must carry the current time");
        assert_eq!(refreshed.action, Action::Claim);
        assert!(
            !reqs.iter().any(|r| r.method == "POST"),
            "refreshing must not add a comment: at half-TTL that is a new \
             bookkeeping comment every 15 minutes, forever"
        );
    }

    #[test]
    fn a_marker_still_well_inside_the_ttl_is_left_alone() {
        // The heartbeat must be due-driven, not every-tick: at the default
        // 60s interval an unconditional refresh would be 1440 writes a day
        // per held issue.
        let server = MockServer::start(vec![
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            MockResponse::json(
                200,
                &one_comment(100, &marker_body(Action::Claim, "me:1", NOW - 10)),
            ),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        seed_synced_item(&conn, &project_id, 7, "started");

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert!(report.ceded.is_empty());
        let reqs = server.requests();
        assert!(
            reqs.iter().all(|r| r.method == "GET"),
            "a fresh marker needs no write, got {reqs:?}"
        );
    }

    #[test]
    fn holding_an_issue_also_refreshes_the_local_ledger_lease() {
        // Two layers expire independently: refreshing only the GitHub marker
        // would leave this instance's OWN agents seeing a stale ledger row
        // and double-claiming work the bridge is still holding.
        let server = MockServer::start(vec![
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            MockResponse::json(
                200,
                &one_comment(100, &marker_body(Action::Claim, "me:1", NOW - 10)),
            ),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        seed_synced_item(&conn, &project_id, 7, "started");
        crate::claims::acquire(
            &ctx.ledger,
            &ctx.repo.to_string(),
            "issue#7",
            "me:1",
            None,
            None,
            NOW - 500,
            1800,
        )
        .unwrap();

        run_once(&ctx, &conn, NOW).unwrap();

        let claims = crate::claims::list(&ctx.ledger, None, true, NOW, 1800).unwrap();
        assert_eq!(claims[0].heartbeat_at, NOW, "the lease must be renewed");
        let _ = server.requests();
    }

    #[test]
    fn a_ceded_issue_can_be_reclaimed_later_and_reuses_its_existing_item() {
        // A cede must be recoverable, not terminal. Step 3 used to skip on
        // the mere existence of a linked item, so the cancelled row left
        // behind by a cede locked this instance out of that issue forever —
        // even once the contention that caused the cede was long gone.
        let server = MockServer::start(vec![
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            MockResponse::json(200, "[]"), // nobody holds it now
            MockResponse::json(201, r#"{"id":100}"#), // our claim comment
            MockResponse::json(
                200,
                &one_comment(100, &marker_body(Action::Claim, "me:1", NOW)),
            ),
            MockResponse::json(200, r#"{"id":100}"#), // marker rewrite
            MockResponse::json(200, "[]"),            // the claimed: label
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        let ceded = seed_synced_item(&conn, &project_id, 7, "cancelled");

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert_eq!(report.claimed, vec![7], "a ceded issue must be reclaimable");
        let linked: Vec<_> = agentflare_backend::item::list_by_project(&conn, &project_id)
            .unwrap()
            .into_iter()
            .filter(|i| i.external_id.as_deref() == Some("7"))
            .collect();
        assert_eq!(
            linked.len(),
            1,
            "reclaiming must re-adopt the existing item, not create a second \
             one — find_by_issue returns the first match, so a duplicate would \
             have the bridge writing one row and reading another"
        );
        assert_eq!(linked[0].id, ceded.id);
        assert!(items::is_active(&conn, &linked[0]), "back to started");
        let _ = server.requests();
    }

    #[test]
    fn our_own_claimed_label_does_not_read_as_a_content_change() {
        // Live-verified defect: claiming adds `claimed:<us>` to the issue,
        // labels fed the content hash, so the very next tick saw a "change"
        // and posted "Progress from …" reporting the bridge's own side
        // effect — one junk comment on every issue it ever claims.
        let with_claim = r#"[{"number":7,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"},{"name":"claimed:me:1"}],"author_association":"OWNER"}]"#;
        let server = MockServer::start(vec![
            MockResponse::json(200, with_claim),
            MockResponse::json(
                200,
                &one_comment(10, &marker_body(Action::Claim, "me:1", NOW - 10)),
            ),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 0), project_id.clone());
        // Hash stored WITHOUT the claimed: label — i.e. what the claim tick
        // recorded before the label existed.
        seed_synced_item(&conn, &project_id, 7, "started");

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert!(
            report.exported.is_empty(),
            "a `claimed:` label is the bridge's own bookkeeping, not issue \
             content — it must not trigger an export"
        );
        let reqs = server.requests();
        assert!(
            reqs.iter().all(|r| r.method == "GET"),
            "and must cause no writes at all, got {reqs:?}"
        );
    }

    #[test]
    fn a_real_label_change_still_reads_as_a_content_change() {
        // The guard rail: filtering `claimed:` must not make the hash blind
        // to labels a HUMAN adds.
        let relabelled = r#"[{"number":7,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"},{"name":"urgent"}],"author_association":"OWNER"}]"#;
        let server = MockServer::start(vec![
            MockResponse::json(200, relabelled),
            MockResponse::json(
                200,
                &one_comment(10, &marker_body(Action::Claim, "me:1", NOW - 10)),
            ),
            MockResponse::json(201, r#"{"id":300}"#), // the progress export
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 0), project_id.clone());
        seed_synced_item(&conn, &project_id, 7, "started");

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert_eq!(report.exported, vec![7], "a real label change must export");
    }

    #[test]
    fn a_failed_close_retries_without_reposting_the_done_comment() {
        // The `done` comment and the close used to share one latch, so a
        // close that failed on its own rolled back a comment that HAD been
        // posted — and the next tick, seeing the same hash dirty again,
        // posted a second "Completed by …". Every failed close added another
        // duplicate, and close is the call most likely to fail alone.
        // A completed item is skipped by step 1 (not active), so the tick is
        // just: queue listing, done comment, close, label removal.
        let open = QUEUE_ONE_ISSUE;
        let server = MockServer::start(vec![
            MockResponse::json(200, open),
            MockResponse::json(201, r#"{"id":300}"#), // the done comment lands
            MockResponse::json(500, r#"{"message":"issue is locked"}"#), // close fails
            MockResponse::json(200, "[]"),            // claimed: label removal
            // tick 2: the issue is still open, so the close is outstanding
            MockResponse::json(200, open),
            MockResponse::json(
                200,
                r#"{"number":7,"html_url":"u","state":"closed","title":"t","body":"","labels":[]}"#,
            ),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 0), project_id.clone());
        let item = seed_synced_item(&conn, &project_id, 7, "started");
        let done = items::state_id_for_group(&conn, &project_id, "completed").unwrap();
        agentflare_backend::item::update_state(&conn, &item.id, &done).unwrap();

        let first = run_once(&ctx, &conn, NOW).unwrap();
        assert_eq!(first.exported, vec![7], "the comment itself succeeded");

        let second = run_once(&ctx, &conn, NOW).unwrap();
        assert!(
            second.exported.is_empty(),
            "the second tick must NOT re-export — the comment already landed"
        );

        let reqs = server.requests();
        let done_comments = reqs
            .iter()
            .filter(|r| r.method == "POST" && r.path.ends_with("/comments"))
            .count();
        assert_eq!(done_comments, 1, "exactly one `done` comment, got {reqs:?}");
        let closes = reqs
            .iter()
            .filter(|r| r.method == "PATCH" && r.path == "/repos/o/r/issues/7")
            .count();
        assert_eq!(closes, 2, "but the close must be retried");
    }

    #[test]
    fn a_failed_export_rolls_the_latch_back_so_the_next_tick_retries() {
        // I4: the stored hash is written BEFORE the comment, so nothing can
        // be posted that we then fail to record. The cost of that ordering is
        // that a failed remote write must not leave the latch advanced, or
        // the export is silently skipped until the content happens to change.
        let ours = one_comment(10, &marker_body(Action::Claim, "me:1", NOW - 10));
        let server = MockServer::start(vec![
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            MockResponse::json(200, &ours), // step 1: we still hold it
            MockResponse::json(500, r#"{"message":"boom"}"#), // the export POST
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            MockResponse::json(200, &ours),
            MockResponse::json(201, r#"{"id":300}"#), // the retry succeeds
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 0), project_id.clone());
        let state = items::state_id_for_group(&conn, &project_id, "started").unwrap();
        agentflare_backend::item::create(
            &conn,
            agentflare_backend::item::CreateItem {
                project_id: project_id.clone(),
                state_id: state,
                name: "needs exporting".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: Some(items::EXTERNAL_SOURCE.to_string()),
                external_id: Some("7".into()),
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap();

        // Tick 1: the POST fails. A hard error, so run_once surfaces it.
        assert!(run_once(&ctx, &conn, NOW).is_err());
        let after_failure = items::find_by_issue(&conn, &project_id, 7).unwrap();
        assert!(
            items::last_hash(&after_failure).is_none(),
            "a failed export must leave the latch where it was, or the retry \
             never happens"
        );

        // Tick 2: same content, and it exports.
        let report = run_once(&ctx, &conn, NOW).unwrap();
        assert_eq!(report.exported, vec![7]);
        let after_success = items::find_by_issue(&conn, &project_id, 7).unwrap();
        assert!(items::last_hash(&after_success).is_some(), "latch advanced");
        let _ = server.requests();
    }

    #[test]
    fn a_failed_cede_comment_leaves_the_item_active_for_the_next_tick() {
        // The mirror of the export latch. Cancelling locally is what stops
        // step 1 re-ceding, so it happens first — but if the announcement
        // then fails, an item left cancelled with no cede marker on the issue
        // reads to every other instance as "still claimed by us".
        let server = MockServer::start(vec![
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            MockResponse::json(200, "[]"), // nobody holds it, so we cede
            MockResponse::json(500, r#"{"message":"boom"}"#), // the cede POST fails
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 0), project_id.clone());
        seed_synced_item(&conn, &project_id, 7, "started");

        assert!(run_once(&ctx, &conn, NOW).is_err());

        let item = items::find_by_issue(&conn, &project_id, 7).unwrap();
        assert!(
            items::is_active(&conn, &item),
            "a cede that never reached GitHub must be retried, not assumed"
        );
        let _ = server.requests();
    }

    #[test]
    fn a_soft_error_is_reported_rather_than_swallowed_whole() {
        // I5: soft errors keep the daemon looping, which is right — but
        // `Forbidden` is a permanently dead bridge, and returning a bare
        // empty report meant nothing ever said so.
        let server = MockServer::start(vec![
            MockResponse::json(403, r#"{"message":"limited"}"#)
                .with_header("x-ratelimit-remaining", "0"),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id);

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert!(report.claimed.is_empty());
        assert!(
            report.soft_error.is_some(),
            "the tick ended early — the runner has to be able to say why"
        );
        let _ = server.requests();
    }

    #[test]
    fn headroom_counts_successful_claims_not_attempts() {
        // I1: with `take(headroom)` the loop was bounded by ATTEMPTS, so a
        // queue whose head was already tracked burned the whole allowance and
        // this instance claimed nothing — every tick, while free issues sat
        // right below the head.
        let queue = r#"[{"number":1,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"}],"author_association":"OWNER"},
                        {"number":2,"html_url":"u","state":"open","title":"t","body":"","labels":[{"name":"agentflare"}],"author_association":"OWNER"}]"#;
        let server = MockServer::start(vec![
            MockResponse::json(200, queue),
            // step 1 re-verifies #1: our own fresh claim, so we keep it and
            // the marker is too young to need a heartbeat.
            MockResponse::json(
                200,
                &one_comment(10, &marker_body(Action::Claim, "me:1", NOW - 10)),
            ),
            // step 3 reaches #2 — free: probe, claim, re-read, name, label.
            MockResponse::json(200, "[]"),
            MockResponse::json(201, r#"{"id":100}"#),
            MockResponse::json(
                200,
                &one_comment(100, &marker_body(Action::Claim, "me:1", NOW)),
            ),
            MockResponse::json(200, r#"{"id":100}"#),
            MockResponse::json(200, "[]"),
        ]);
        let (conn, project_id) = test_db();
        // max_claims 2, one already held → exactly one claim of headroom.
        let ctx = ctx_with_project(test_ctx(&server, 2), project_id.clone());
        seed_synced_item(&conn, &project_id, 1, "started");

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert_eq!(
            report.claimed,
            vec![2],
            "the one unit of headroom must reach a free issue, not be spent \
             on the already-tracked queue head"
        );
        let _ = server.requests();
    }

    #[test]
    fn a_ceded_item_is_not_exported_onto_an_issue_someone_else_now_owns() {
        // I2: step 2 had no guard, so a cancelled-but-still-linked item
        // reached export_if_dirty and posted "Progress from …" on an issue
        // the winner now holds. It always drifts: labels feed the hash, and
        // the winner adds its own `claimed:` label.
        let server = MockServer::start(vec![MockResponse::json(200, QUEUE_ONE_ISSUE)]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 0), project_id.clone());
        // Deliberately out of sync (no stored hash) so export WOULD fire.
        let state = items::state_id_for_group(&conn, &project_id, "cancelled").unwrap();
        agentflare_backend::item::create(
            &conn,
            agentflare_backend::item::CreateItem {
                project_id: project_id.clone(),
                state_id: state,
                name: "drifted".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: Some(items::EXTERNAL_SOURCE.to_string()),
                external_id: Some("7".into()),
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap();

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert!(report.exported.is_empty());
        let reqs = server.requests();
        assert_eq!(
            reqs.len(),
            1,
            "only the queue listing — a ceded item must write nothing, got {reqs:?}"
        );
    }

    #[test]
    fn a_completed_item_is_still_exported_after_a_cede_guard_is_added() {
        // The other half of I2: the guard must be `is_ceded`, not
        // `!is_active`. A completed item is also "not active", but it still
        // owes its issue the `done` marker and the close that follows —
        // guarding on `!is_active` would strand finished work forever.
        let server = MockServer::start(vec![
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            MockResponse::json(201, r#"{"id":300}"#), // the `done` comment
            MockResponse::json(
                200,
                r#"{"number":7,"html_url":"u","state":"closed","title":"t","body":"","labels":[]}"#,
            ),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 0), project_id.clone());
        // Via `update_state`, not `create`: only the transition sets
        // `completed_at`, and that is what makes the export a `done`.
        let item = seed_synced_item(&conn, &project_id, 7, "started");
        let done_state = items::state_id_for_group(&conn, &project_id, "completed").unwrap();
        agentflare_backend::item::update_state(&conn, &item.id, &done_state).unwrap();

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert_eq!(report.exported, vec![7], "finished work must still export");
        let reqs = server.requests();
        let posted = reqs.iter().find(|r| r.method == "POST").unwrap();
        let sent: serde_json::Value = serde_json::from_str(&posted.body).unwrap();
        let marker = Marker::parse(sent["body"].as_str().unwrap()).unwrap();
        assert_eq!(marker.action, Action::Done);
    }

    #[test]
    fn our_own_orphan_claim_is_adopted_rather_than_waited_out() {
        // I3: `resolve_holder(...).is_some()` didn't check WHO. If GitHub
        // said we held the issue but we had no local item — a Transport blip
        // after posting the claim, or the db loss marker.rs advertises as
        // recoverable — we bailed and sat out a full TTL waiting for our own
        // marker to expire.
        let server = MockServer::start(vec![
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            // Our claim from a previous, half-completed attempt.
            MockResponse::json(
                200,
                &one_comment(100, &marker_body(Action::Claim, "me:1", NOW - 10)),
            ),
            MockResponse::json(200, r#"{"id":100}"#), // marker rewrite
            MockResponse::json(200, "[]"),            // the claimed: label
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert_eq!(report.claimed, vec![7], "we already hold it — adopt it");
        let item = items::find_by_issue(&conn, &project_id, 7).unwrap();
        assert!(items::is_active(&conn, &item));
        let reqs = server.requests();
        assert!(
            !reqs
                .iter()
                .any(|r| r.method == "POST" && r.path.ends_with("/comments")),
            "adopting must not post a second claim comment, got {reqs:?}"
        );
    }

    #[test]
    fn a_live_claim_by_another_instance_is_still_left_alone() {
        // The guard rail on the I3 fix: only OUR marker may be adopted.
        let server = MockServer::start(vec![
            MockResponse::json(200, QUEUE_ONE_ISSUE),
            MockResponse::json(
                200,
                &one_comment(50, &marker_body(Action::Claim, "other:9", NOW)),
            ),
        ]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert!(report.claimed.is_empty());
        assert!(items::find_by_issue(&conn, &project_id, 7).is_none());
        let _ = server.requests();
    }

    #[test]
    fn a_completed_item_is_not_reclaimed_while_its_done_export_is_pending() {
        // `is_ceded` is deliberately narrower than `!is_active`: between
        // completing an item and exporting its `done`, the issue is still
        // open and still in the queue. Treating "not active" as "re-claimable"
        // would re-open work we had just finished.
        let server = MockServer::start(vec![MockResponse::json(200, QUEUE_ONE_ISSUE)]);
        let (conn, project_id) = test_db();
        let ctx = ctx_with_project(test_ctx(&server, 3), project_id.clone());
        seed_synced_item(&conn, &project_id, 7, "completed");

        let report = run_once(&ctx, &conn, NOW).unwrap();

        assert!(report.claimed.is_empty(), "must not re-claim finished work");
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1, "only the queue listing, got {reqs:?}");
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
