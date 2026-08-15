use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::error::Result;

use super::crud::{get, update, update_state};
use super::{UpdateItem, now};

/// Outcome of a claim attempt — the raw lease `Acquire` plus the handoff
/// freeze rule: while an item carries an `assignee_agent` that nobody has
/// claimed yet (a handoff sitting unaccepted), only that assignee may
/// acquire it. Once any claim has ever been taken (even a since-stale one),
/// the ordinary `Acquired`/`Held` staleness rules take back over — this
/// variant only covers the fresh, never-claimed window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Acquired,
    Held { owner: String, age_secs: i64 },
    BlockedByAssignee { assignee: String },
}

/// Canonical agent identity of an owner id (`<agent>:<instance>` ->
/// canonical `<agent>`), matching `assignee_agent`'s canonical form —
/// `assignee_agent` is canonicalized on write (see `create`/`update`), but
/// `owner` is the raw caller-supplied id, so an alias like `claude:1` must
/// be canonicalized here too or it won't match `claude-code`.
///
/// `pub` because `assignee_agent` legitimately carries the instance suffix
/// after a claim (`claim()` below stores the raw `owner`, on purpose — see
/// its own doc comment and the tests pinning that), so any caller outside
/// this module that reads `assignee_agent` back to resolve *which agent
/// type* it names (not which specific instance) needs the same stripping
/// this module already does internally, instead of re-deriving it.
pub fn agent_part(owner: &str) -> String {
    agent_registry::canonicalize(owner.split(':').next().unwrap_or(owner))
}

/// Claims an item so other agents don't duplicate the work: on a fresh
/// acquire, sets the assignee and moves state into the project's "started"
/// group (which sets `started_at`, via `update_state`). A live claim held by
/// someone else returns `Held` and leaves the item untouched. An item
/// freshly handed off (assignee set, never yet claimed) to a *different*
/// agent than the caller returns `BlockedByAssignee` instead of letting the
/// caller silently steal it. Acquisition, the state transition, and the
/// assignee update are one transaction — a mid-sequence failure can't leave
/// `item_claims` saying "claimed" while the item itself never reflects it.
pub fn claim(
    conn: &Connection,
    item_id: &str,
    owner: &str,
    now: i64,
    ttl_secs: i64,
) -> Result<ClaimOutcome> {
    // IMMEDIATE, not the default DEFERRED: this transaction always ends in a
    // write, and DEFERRED takes its read snapshot on the first SELECT below,
    // then only grabs the write lock later. Under concurrent writers that
    // window lets another connection's commit age out the snapshot, and the
    // write attempt fails with SQLITE_BUSY_SNAPSHOT — an error the
    // busy_timeout retry loop does not cover (retrying can't fix a stale
    // snapshot, only restarting the transaction can), so it surfaces
    // instantly as "database is locked" instead of waiting its turn like
    // ordinary lock contention does. Taking the write lock upfront closes
    // that window and puts this claim's lock wait through the normal,
    // busy_timeout-honoring path.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let item = get(&tx, item_id)?;
    if let Some(assignee) = &item.assignee_agent
        && agent_part(assignee) != agent_part(owner)
        && crate::claim::current_owner(&tx, item_id).is_none()
    {
        // Excludes completed/cancelled items: a done-and-released item is
        // fair game for anyone to re-claim (e.g. reopened follow-up work) —
        // the freeze only protects a handoff that's still open.
        let state = crate::state::get(&tx, &item.state_id)?;
        if !matches!(state.group_name.as_str(), "completed" | "cancelled") {
            return Ok(ClaimOutcome::BlockedByAssignee {
                assignee: assignee.clone(),
            });
        }
    }
    let ttl_secs = crate::claim::effective_ttl_secs(&tx, item_id, ttl_secs);
    let outcome = crate::claim::acquire(&tx, item_id, owner, now, ttl_secs)?;
    let result = match outcome {
        crate::claim::Acquire::Acquired => {
            let started_state = crate::state::first_in_group(&tx, &item.project_id, "started")?;
            update_state(&tx, item_id, &started_state.id)?;
            update(
                &tx,
                item_id,
                UpdateItem {
                    assignee_agent: Some(owner.to_string()),
                    ..Default::default()
                },
            )?;
            ClaimOutcome::Acquired
        }
        crate::claim::Acquire::Held { owner, age_secs } => ClaimOutcome::Held { owner, age_secs },
    };
    tx.commit()?;
    Ok(result)
}

/// Moves a claimed item into the project's "completed" group WITHOUT
/// releasing the claim lease yet. Deliberately split from the lease release
/// (contrast with the old `claim_done`, which did both atomically): the
/// `"done"` MCP arm calls this, then runs `worktree::push_and_open_pr`
/// (which needs the lease to still look held so a concurrent `claim()` on
/// the same item between mark_completed and the deferred release below is
/// still correctly rejected), and only *after* publish releases the lease
/// via `claim::done`. Returns `Ok(true)` when the item was actually moved
/// to completed, `Ok(false)` when the caller doesn't own the claim.
pub fn mark_completed(conn: &Connection, item_id: &str, owner: &str) -> Result<bool> {
    // One transaction start to finish so the ownership check can't go stale
    // between the guard and the write — without this, a concurrent
    // release()+claim() by a different owner could slip in between the
    // check and update_state below, completing the item out from under its
    // new owner.
    let tx = conn.unchecked_transaction()?;
    if !crate::claim::is_owner(&tx, item_id, owner)? {
        return Ok(false);
    }
    let item = get(&tx, item_id)?;
    let completed_state = crate::state::first_in_group(&tx, &item.project_id, "completed")?;
    update_state(&tx, item_id, &completed_state.id)?;
    tx.commit()?;
    // Keep the claim lease held for the MCP caller's deferred release.
    Ok(true)
}

/// Moves a claimed item into the project's "in_review" group, same shape and
/// same claim-lease-stays-held contract as `mark_completed` above — used
/// instead of it when `done` results in an open PR (item #420). The work
/// isn't actually finished until that PR merges: landing straight on
/// "completed" would show the item as done while its PR is still red or
/// under review, which is the state-side half of the bug `mark_completed`
/// alone had (the other half was deleting the worktree too, fixed in
/// `mcp_server::item::item_done` by only cleaning up when no PR resulted).
///
/// Auto-creates the "in_review" state on first use per project: it's in
/// `state::DEFAULT_STATES` for every project seeded after item #420, but
/// existing projects were seeded before it existed and have no such state
/// to find.
pub fn mark_in_review(conn: &Connection, item_id: &str, owner: &str) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    if !crate::claim::is_owner(&tx, item_id, owner)? {
        return Ok(false);
    }
    let item = get(&tx, item_id)?;
    let review_state = match crate::state::first_in_group(&tx, &item.project_id, "in_review") {
        Ok(s) => s,
        Err(crate::error::Error::NotFound(_)) => crate::state::create(
            &tx,
            crate::state::CreateState {
                project_id: item.project_id.clone(),
                name: crate::state::IN_REVIEW_STATE_NAME.into(),
                group_name: "in_review".into(),
                sequence: crate::state::IN_REVIEW_STATE_SEQUENCE,
                is_default: None,
                color: Some(crate::state::IN_REVIEW_STATE_COLOR.into()),
            },
        )?,
        Err(e) => return Err(e),
    };
    update_state(&tx, item_id, &review_state.id)?;
    tx.commit()?;
    Ok(true)
}

/// Promotes an item from "in_review" to "completed" once its PR is
/// confirmed merged (`item_check_merge`, item #420). Unlike
/// `mark_completed`/`mark_in_review`, this is deliberately NOT owner-scoped:
/// `item_done` leaves the claim lease held (not released) when it moves an
/// item into "in_review" specifically so nobody else can claim it out from
/// under the pending review, so by the time anything reaches this function
/// no other owner could legally exist — whoever notices the merge and calls
/// `check_merge`, possibly a different session than the one that opened the
/// PR, is allowed to finish the transition. Releases whatever lease is
/// still held as part of the same commit. Returns `Ok(false)` (a no-op,
/// not an error) when the item isn't currently in "in_review" — callers
/// can call this speculatively without checking state first.
pub fn promote_in_review_to_completed(conn: &Connection, item_id: &str) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let item = get(&tx, item_id)?;
    let state = crate::state::get(&tx, &item.state_id)?;
    if state.group_name != "in_review" {
        return Ok(false);
    }
    let completed_state = crate::state::first_in_group(&tx, &item.project_id, "completed")?;
    update_state(&tx, item_id, &completed_state.id)?;
    if let Some(owner) = crate::claim::current_owner(&tx, item_id) {
        crate::claim::done(&tx, item_id, &owner, now())?;
    }
    tx.commit()?;
    Ok(true)
}
