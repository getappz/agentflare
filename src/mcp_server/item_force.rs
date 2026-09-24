//! Overriding a claim whose TTL hasn't expired but whose owner is provably
//! gone (item #639). Two paths, neither a blanket unlock:
//!
//! - `auto_release_dead_claims`: supervisor sweep that releases a claim once
//!   its owner's job is recorded terminal in the job queue AND a terminal
//!   `agentflare work — failed` comment was posted since the claim was taken.
//! - `force_takeover`: `item(release|done|check_merge, force=true,
//!   force_reason=...)` for cases the sweep can't reach, refused unless at
//!   least one piece of dead-claim evidence holds. Every override is logged
//!   as an item comment so it's auditable, never silent.
//!
//! Live incident: image-qc item #299 — its job made real commits, failed PR
//! creation, posted the terminal failure comment and vanished, leaving a
//! fixed-and-pushed item un-completable for ~2h45m of remaining TTL.

use super::*;
use crate::dispatch_failure_ceiling::WORK_FAILURE_MARKER;

/// Prefix on the audit comment `force_takeover` posts.
pub(crate) const FORCE_OVERRIDE_MARKER: &str = "## agentflare — forced claim override";
/// Prefix on the comment `auto_release_dead_claims` posts.
pub(crate) const AUTO_RELEASE_MARKER: &str = "## supervisor — auto-released dead claim";

/// Adds `forced_override` (the gate evidence) to a response when one happened.
pub(super) fn with_forced(mut resp: serde_json::Value, forced: Option<String>) -> String {
    if let Some(evidence) = forced {
        resp["forced_override"] = serde_json::Value::String(evidence);
    }
    resp.to_string()
}

/// `Some(reason)` when the caller asked for `force=true` — which requires a
/// non-empty `force_reason`, mirroring `send`'s task-mode `force` +
/// `force_reason` convention. `None` when force wasn't requested.
fn requested_force_reason(
    force: Option<bool>,
    force_reason: Option<&str>,
) -> Result<Option<String>, ErrorData> {
    if force != Some(true) {
        return Ok(None);
    }
    force_reason
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(|r| Some(r.to_string()))
        .ok_or_else(|| {
            ErrorData::invalid_params(
                "force=true requires a non-empty force_reason (it is logged as an audit comment on the item)",
                None,
            )
        })
}

/// A live (not stale, not done) claim on `item_id` held by someone other
/// than `owner`, judged by the same effective TTL `claim`/`release` use.
fn live_foreign_claim(
    conn: &rusqlite::Connection,
    item_id: &str,
    owner: &str,
    now: i64,
    ttl: i64,
) -> rusqlite::Result<Option<db_kit::claim::Claim>> {
    let ttl = agentflare_backend::claim::effective_ttl_secs(conn, item_id, ttl);
    Ok(agentflare_backend::claim::list_all(conn, now, ttl)?
        .into_iter()
        .find(|c| c.key == [item_id] && c.status == "claimed" && !c.stale && c.owner != owner))
}

/// Evidence that `holder`'s job is dead: its `<agent>:<job-id>` instance is a
/// job the queue records as terminal. Anything else (unknown id, pid/session
/// instance, queue unavailable) is NOT confirmation.
fn owner_job_dead(queue: Option<&agentflare_jobs::Queue>, holder: &str) -> Option<String> {
    let (_, job_id) = holder.split_once(':')?;
    let info = queue?.get(job_id).ok()?;
    info.state
        .is_terminal()
        .then(|| format!("owner job {job_id} is {:?}", info.state))
}

/// A terminal work-failure comment posted at or after `since` (the current
/// claim's acquisition) — an older failure from a previous dispatch cycle
/// says nothing about the claim held now.
fn terminal_failure_since(conn: &rusqlite::Connection, item_id: &str, since: i64) -> bool {
    agentflare_backend::comment::list_by_item(conn, item_id).is_ok_and(|comments| {
        comments
            .iter()
            .any(|c| c.created_at >= since && c.body.starts_with(WORK_FAILURE_MARKER))
    })
}

/// The item's own PR is pushed and verified (CI passing, or already merged).
fn branch_verified(
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
) -> Option<String> {
    match crate::worktree::pr_ci_status(item, repo_root) {
        crate::worktree::PrCiStatus::Passing { number, .. } => {
            Some(format!("PR #{number} is pushed with passing CI"))
        }
        crate::worktree::PrCiStatus::Merged => Some("the item's PR is merged".to_string()),
        _ => None,
    }
}

impl AgentflareMcp {
    /// Entry point for `release|done|check_merge`: validates `force` +
    /// `force_reason`, then runs [`Self::force_takeover`] as the caller.
    pub(super) fn force_if_requested(
        &self,
        force: Option<bool>,
        force_reason: Option<&str>,
        raw_id: &str,
        action: &str,
    ) -> Result<Option<String>, ErrorData> {
        let Some(reason) = requested_force_reason(force, force_reason)? else {
            return Ok(None);
        };
        self.force_takeover(
            raw_id,
            &crate::claims::owner_id(),
            crate::claims::now(),
            backend_claim_ttl_secs(),
            &reason,
            action,
        )
    }

    /// Transfers a live claim held by someone else to `owner`, but only with
    /// dead-claim evidence. Returns `Ok(None)` when there was no live foreign
    /// claim to override (the caller's normal path, including its own
    /// stale-TTL steal, handles that), `Ok(Some(evidence))` on a successful
    /// override, and an error when the gate refuses.
    fn force_takeover(
        &self,
        raw_id: &str,
        owner: &str,
        now: i64,
        ttl: i64,
        reason: &str,
        action: &str,
    ) -> Result<Option<String>, ErrorData> {
        let item_id = &self.with_backend_db(|conn| self.resolve_item_id(conn, raw_id))??;
        let found = self.with_backend_db(|conn| {
            let Some(claim) = live_foreign_claim(conn, item_id, owner, now, ttl)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            else {
                return Ok(None);
            };
            let item = agentflare_backend::item::get(conn, item_id).map_err(map_backend_err)?;
            let failed = terminal_failure_since(conn, item_id, claim.created_at);
            Ok::<_, ErrorData>(Some((claim.owner, item, failed)))
        })??;
        let Some((holder, item, failed)) = found else {
            return Ok(None);
        };
        // Cheapest evidence first; the PR check is a network round trip and
        // runs outside the backend DB lock.
        let queue = self.job_queue().ok().flatten();
        let evidence = owner_job_dead(queue.as_ref(), &holder)
            .or_else(|| {
                failed.then(|| {
                    "terminal `agentflare work — failed` comment posted since the claim was taken"
                        .to_string()
                })
            })
            .or_else(|| branch_verified(&item, &self.worktree_repo_root()))
            .ok_or_else(|| {
                ErrorData::invalid_params(
                    format!(
                        "force refused: item {item_id} is claimed by '{holder}' and no override \
                         condition holds (owner job confirmed dead, terminal failure comment \
                         posted since the claim was taken, or the item's PR pushed with \
                         passing/merged CI) -- wait out the TTL or have the owner release"
                    ),
                    None,
                )
            })?;
        let body = format!(
            "{FORCE_OVERRIDE_MARKER}\n\n- action: `{action}`\n- prior owner: `{holder}`\n- new \
             owner: `{owner}`\n- evidence: {evidence}\n- reason: {reason}"
        );
        self.with_backend_db(|conn| {
            // Owner-scoped: only ever drops `holder`'s own row.
            agentflare_backend::item::release(conn, item_id, &holder).map_err(map_backend_err)?;
            if let agentflare_backend::claim::Acquire::Held { owner: other, .. } =
                agentflare_backend::claim::acquire(conn, item_id, owner, now, ttl)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            {
                return Err(ErrorData::invalid_params(
                    format!("item {item_id} was re-claimed by '{other}' during the override"),
                    None,
                ));
            }
            agentflare_backend::comment::create(
                conn,
                item_id,
                crate::claims::agent_of(owner),
                &body,
            )
            .map_err(map_backend_err)?;
            Ok(())
        })??;
        Ok(Some(evidence))
    }

    /// `check_merge force=true` on an item that never reached in_review
    /// (its `done` failed after the PR went up): promote it straight to
    /// completed once its PR is confirmed merged — the same promotion side
    /// effects as the normal in_review path.
    pub(super) fn force_complete_merged(
        &self,
        item_id: &str,
        item: &agentflare_backend::item::Item,
        forced: Option<String>,
    ) -> Result<String, ErrorData> {
        let owner = &crate::claims::owner_id();
        let repo_root = self.worktree_repo_root();
        if !crate::worktree::is_pr_merged(item, &repo_root) {
            return Ok(serde_json::json!({
                "item_id": item_id,
                "promoted": false,
                "reason": "item is not in_review and its PR is not merged",
            })
            .to_string());
        }
        let now = crate::claims::now();
        let promoted = self.with_backend_db(|conn| {
            // `force_takeover` already moved any live foreign claim to us;
            // this covers an item with no live claim at all.
            agentflare_backend::claim::acquire(conn, item_id, owner, now, backend_claim_ttl_secs())
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            let promoted = agentflare_backend::item::mark_completed(conn, item_id, owner)
                .map_err(map_backend_err)?;
            if promoted {
                let _ = agentflare_backend::claim::done(conn, item_id, owner, now);
                crate::supervisor::cascade_unblock_dependents(conn, item_id);
            }
            Ok::<_, ErrorData>(promoted)
        })??;
        if promoted {
            crate::worktree::cleanup_worktree(item, &repo_root);
            crate::worktree::relabel_pr_completed(item, &repo_root);
        }
        let resp = serde_json::json!({"item_id": item_id, "promoted": promoted});
        Ok(with_forced(resp, forced))
    }
}

/// Supervisor sweep: releases every live claim whose owner's job is
/// confirmed dead AND that carries a terminal failure comment posted since
/// it was taken — no `force` call needed. Leaves the worktree alone (it may
/// hold committed work someone will finish). Returns how many were released.
pub(crate) fn auto_release_dead_claims(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
) -> usize {
    let now = crate::claims::now();
    let ttl = backend_claim_ttl_secs();
    let candidates = mcp
        .with_backend_db(|conn| {
            agentflare_backend::claim::list_all(conn, now, ttl)
                .unwrap_or_default()
                .into_iter()
                .filter(|c| c.status == "claimed" && !c.stale && c.key.len() == 1)
                .filter(|c| terminal_failure_since(conn, &c.key[0], c.created_at))
                .map(|c| (c.key[0].clone(), c.owner))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut released = 0;
    for (item_id, holder) in candidates {
        let Some(evidence) = owner_job_dead(Some(queue), &holder) else {
            continue;
        };
        let ok = mcp
            .with_backend_db(|conn| {
                let ok =
                    agentflare_backend::item::release(conn, &item_id, &holder).unwrap_or(false);
                if ok {
                    let body = format!(
                        "{AUTO_RELEASE_MARKER}\n\nReleased `{holder}`'s claim before its TTL: \
                         {evidence}, and a terminal failure comment was posted since the claim \
                         was taken."
                    );
                    let _ =
                        agentflare_backend::comment::create(conn, &item_id, "supervisor", &body);
                }
                ok
            })
            .unwrap_or(false);
        released += usize::from(ok);
    }
    if released > 0 {
        eprintln!("agentflare-supervisor: auto-released {released} dead claim(s)");
    }
    released
}
