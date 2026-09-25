//! Review-bot thread follow-up for the PRs the sweep drives (CodeRabbit by
//! default; `[review_sweep].review_bots` for others). One sweep does, per
//! CI-green PR:
//!
//! 1. Fetches every review thread with its comments and keeps the unresolved
//!    ones a bot opened (`review_findings::classify_threads`).
//! 2. For threads the agent already reported on (`item(action="review_result")`),
//!    confirms a "fixed" sha reached the remote, replies on the thread with
//!    the outcome, and resolves it -- fixed threads only, and only after the
//!    reply landed. Not-valid / out-of-scope replies stay open for the bot.
//! 3. Escalates a thread that keeps coming back past `max_rounds` to the
//!    needs-human gate instead of looping.
//! 4. Dispatches one structured fix task for the rest
//!    (`coderabbit_repair_or_gate`, moved here from `supervisor.rs`).
//! 5. Nudges a bot that auto-paused on a busy branch (`@coderabbitai review`,
//!    once per head).
//!
//! The merge gate reads the result: no auto-merge while a non-optional bot
//! thread is unresolved, unless it holds an out-of-scope reply the bot
//! accepted.

use super::*;
use crate::github::review_threads::{self, ReviewThread};
use crate::github::{Client, RepoId};

pub(crate) use super::review_findings::*;

/// Marker prefix on a review-repair-dispatch comment -- counted the same
/// way `CI_SELF_REPAIR_MARKER` is, against the same
/// `quota::decide::SELF_REPAIR_CAP`, so a PR a bot keeps flagging doesn't
/// retry-dispatch forever either. The wording predates configurable bots
/// and stays: it is what earlier dispatches on live items already carry.
pub(super) const CODERABBIT_REPAIR_MARKER: &str =
    "## supervisor — CodeRabbit review repair dispatched";

/// Marker prefix on the one-time repair summary posted once the findings that
/// triggered a dispatch are all resolved (`maybe_post_repair_complete_summary`).
pub(super) const CODERABBIT_REPAIR_COMPLETE_MARKER: &str =
    "## supervisor — CodeRabbit review repair complete";

/// Item-metadata keys tracking review-repair announcements (item #633
/// follow-up): the findings snapshot is fingerprinted and the dispatch post
/// goes out once per snapshot; retries re-dispatch silently.
pub(super) const CODERABBIT_REPAIR_ANNOUNCED_KEY: &str = "coderabbit_repair_announced_for";
/// Silent re-dispatches (same findings, no new marker comment). The cap counts
/// marker comments + this counter, so retries still trip it exactly as before.
pub(super) const CODERABBIT_REPAIR_SILENT_KEY: &str = "coderabbit_repair_silent_attempts";
/// Fingerprint whose completion summary was already posted.
pub(super) const CODERABBIT_REPAIR_COMPLETED_KEY: &str = "coderabbit_repair_completed_for";

/// Marker on the item comment recording a thread escalated to a human.
pub(super) const REVIEW_THREAD_ESCALATED_MARKER: &str = "## supervisor — review thread escalated";

/// Stable fingerprint of a findings snapshot: sorted
/// `thread:round:path:line:first-line` rows, truncated. Sorted (not fetch
/// order) so reordered fetches don't re-announce; the round is in so a
/// bot's pushback re-announces, and the body's first line so an edited
/// finding does.
pub(super) fn coderabbit_findings_fingerprint(findings: &[BotFinding]) -> String {
    let mut rows: Vec<String> = findings
        .iter()
        .map(|f| {
            let first = f.body.lines().next().unwrap_or("").trim();
            format!("{}:{}:{}:{first}", f.thread_id, f.round(), f.location())
        })
        .collect();
    rows.sort();
    rows.join("\n").chars().take(2000).collect()
}

/// What one sweep of a PR's bot threads left for the merge gate.
#[derive(Debug, Default)]
pub(crate) struct ReviewBotState {
    /// Findings whose next round is the agent's to work: dispatched by
    /// `coderabbit_repair_or_gate` when at least one of them is not
    /// optional (`dispatch_needed`); optional ones ride along, never start
    /// a repair on their own.
    pub to_dispatch: Vec<BotFinding>,
    /// Unresolved, non-optional threads not yet accepted by the bot --
    /// these hold the merge whatever else happens this tick.
    pub blocking: usize,
    /// Threads replied to (and, when fixed, resolved) this sweep.
    pub replied: usize,
    /// Reported-fixed threads whose sha isn't on the remote yet.
    pub waiting_push: usize,
    /// Threads at or past `max_rounds`, gated for a human.
    pub escalated: usize,
    /// The thread fetch failed (rate limit, transport, no client): the
    /// review state is unknown, so the merge is held rather than treated
    /// as clean.
    pub unknown: bool,
}

impl ReviewBotState {
    /// A state that only asks for a dispatch -- what the repair tests build.
    #[cfg(test)]
    pub(crate) fn for_dispatch(findings: Vec<BotFinding>) -> Self {
        let blocking = findings.iter().filter(|f| f.blocks_merge()).count();
        ReviewBotState {
            to_dispatch: findings,
            blocking,
            ..Default::default()
        }
    }

    /// Whether a repair job is worth starting: some actionable finding is
    /// not optional. A PR carrying only nitpicks merges (once approved)
    /// instead of bouncing to an agent for them.
    pub fn dispatch_needed(&self) -> bool {
        self.to_dispatch.iter().any(|f| !f.optional())
    }

    pub fn blocks_merge(&self) -> bool {
        self.unknown || self.blocking > 0 || self.dispatch_needed()
    }

    /// The state for a sweep that could not look: holds the merge.
    fn unknown() -> Self {
        ReviewBotState {
            unknown: true,
            ..Default::default()
        }
    }
}

/// The per-PR inputs `sweep_review_threads` needs beyond the client.
pub(crate) struct ReviewSweepInput<'a> {
    pub item: &'a agentflare_backend::item::Item,
    pub number: u64,
    pub head_sha: Option<&'a str>,
    pub labels: &'a [String],
    pub label_id_by_name: &'a std::collections::HashMap<String, String>,
    pub folder_path: &'a str,
}

/// Resolves the repo and a live client, then runs `sweep_review_threads`.
/// Soft-fails to an empty state (nothing to dispatch, nothing blocking) on
/// any lookup failure, same as every other GitHub-touching helper in the
/// sweep: the caller's fallback is to skip this tick and try again.
#[allow(clippy::too_many_arguments)]
pub(super) fn fetch_review_bot_state(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
    number: u64,
    head_sha: Option<&str>,
    labels: &[String],
    label_id_by_name: &std::collections::HashMap<String, String>,
    folder_path: &str,
) -> ReviewBotState {
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return ReviewBotState::unknown();
    };
    let Ok(client) = Client::new() else {
        return ReviewBotState::unknown();
    };
    let cfg = ReviewBotConfig::load(repo_root);
    sweep_review_threads(
        &client,
        &repo,
        mcp,
        queue,
        &cfg,
        ReviewSweepInput {
            item,
            number,
            head_sha,
            labels,
            label_id_by_name,
            folder_path,
        },
    )
}

fn current_metadata(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
) -> serde_json::Map<String, serde_json::Value> {
    // Re-read, not the sweep's snapshot: the agent's `review_result` calls
    // land between the snapshot and this sweep.
    let fresh = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item.id))
        .ok()
        .and_then(Result::ok)
        .map(|i| i.metadata)
        .unwrap_or_else(|| item.metadata.clone());
    crate::mcp_server::metadata_object(&fresh)
}

fn update_metadata(
    mcp: &AgentflareMcp,
    item_id: &str,
    merge: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> bool {
    mcp.with_backend_db(|conn| crate::mcp_server::merge_item_metadata(conn, item_id, merge))
        .ok()
        .and_then(Result::ok)
        .is_some()
}

/// One sweep of `input.number`'s bot threads (see the module doc). Takes the
/// client so tests can point it at a mock server.
pub(crate) fn sweep_review_threads(
    client: &Client,
    repo: &RepoId,
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    cfg: &ReviewBotConfig,
    input: ReviewSweepInput<'_>,
) -> ReviewBotState {
    let item = input.item;
    let mut state = ReviewBotState::default();
    let threads = match review_threads::list_review_threads(client, repo, input.number) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "agentflare-supervisor: could not list review threads on PR #{}: {}",
                input.number,
                e.log_safe()
            );
            state.unknown = true;
            return state;
        }
    };
    let by_id: std::collections::HashMap<&str, &ReviewThread> =
        threads.iter().map(|t| (t.id.as_str(), t)).collect();
    let meta = current_metadata(mcp, item);
    let findings = classify_threads(&threads, cfg, &meta);
    let in_flight = job_in_flight(queue, &item.id);
    let mut commits: Option<Vec<String>> = None;

    // A restart between a fix's reply and its resolve leaves the thread
    // open with our marker as its last word: finish the resolve, never
    // repeat the reply. Only when that marker really is the last word --
    // a human who answered after it has reopened the thread, and a marker
    // the item's records don't vouch for is not ours.
    for f in &findings {
        let last_is_our_fix = by_id.get(f.thread_id.as_str()).is_some_and(|t| {
            t.comments
                .last()
                .and_then(|c| ReplyMarker::parse(&c.body))
                .is_some_and(|m| {
                    m.outcome == ReviewOutcome::Fixed && marker_trusted(&m, &t.id, &meta)
                })
        });
        if matches!(
            f.status,
            ThreadStatus::Waiting {
                outcome: ReviewOutcome::Fixed
            }
        ) && last_is_our_fix
        {
            match review_threads::resolve_review_thread(client, &f.thread_id) {
                Ok(()) => state.replied += 1,
                Err(e) => eprintln!(
                    "agentflare-supervisor: could not resolve thread {} of PR #{}: {}",
                    f.thread_id,
                    input.number,
                    e.log_safe()
                ),
            }
        }
    }

    for f in findings.iter().filter(|f| f.actionable()) {
        let next = f.round();
        let record = thread_record(&meta, &f.thread_id);
        let Some(thread) = by_id.get(f.thread_id.as_str()) else {
            continue;
        };
        if let Some(result) = thread_result(&meta, &f.thread_id).filter(|r| r.round.max(1) == next)
        {
            if result.outcome == ReviewOutcome::Fixed {
                // A failed commit list is not an empty one: the fix may well
                // be on the branch, so the thread waits for the next tick
                // instead of having its report discarded.
                if commits.is_none() {
                    match review_threads::pr_commit_shas(client, repo, input.number) {
                        Ok(s) => commits = Some(s),
                        Err(e) => {
                            eprintln!(
                                "agentflare-supervisor: could not list commits of PR #{}: {}",
                                input.number,
                                e.log_safe()
                            );
                            state.waiting_push += 1;
                            continue;
                        }
                    }
                }
                let shas = commits.as_deref().unwrap_or_default();
                let pushed = result
                    .sha
                    .as_deref()
                    .is_some_and(|s| sha_on_remote(s, shas));
                if !pushed {
                    if in_flight {
                        // The agent may still be pushing.
                        state.waiting_push += 1;
                        continue;
                    }
                    // The job is over and the commit never arrived: the
                    // report is stale, so the thread goes back to the agent.
                    let _ = mcp.comment_impl(CommentRequest {
                        action: "create".into(),
                        item_id: Some(item.id.clone()),
                        body: Some(format!(
                            "## supervisor — review result discarded\n\nThread `{}` was reported \
                             fixed in `{}`, but that commit is not on PR #{}'s branch. The \
                             thread goes back to the agent on the next dispatch.",
                            f.thread_id,
                            result.sha.as_deref().unwrap_or("?"),
                            input.number
                        )),
                        ..Default::default()
                    });
                    update_metadata(mcp, &item.id, |m| remove_thread_result(m, &f.thread_id));
                    state.to_dispatch.push(f.clone());
                    continue;
                }
            }
            match reply_and_settle(client, repo, input.number, thread, next, &result) {
                Ok(()) => {
                    update_metadata(mcp, &item.id, |m| {
                        set_thread_replied(m, &f.thread_id, next);
                        remove_thread_result(m, &f.thread_id);
                    });
                    state.replied += 1;
                }
                Err(e) => eprintln!(
                    "agentflare-supervisor: could not reply on thread {} of PR #{}: {}",
                    f.thread_id,
                    input.number,
                    e.log_safe()
                ),
            }
            continue;
        }
        if record.escalated {
            state.escalated += 1;
            continue;
        }
        if next > cfg.max_rounds {
            escalate_thread(mcp, &input, f, cfg.max_rounds);
            state.escalated += 1;
            continue;
        }
        if record.round >= next && in_flight {
            // Dispatched for this round and the agent is still on it.
            continue;
        }
        state.to_dispatch.push(f.clone());
    }

    state.blocking = findings.iter().filter(|f| f.blocks_merge()).count();
    post_detached_results(client, repo, mcp, item, input.number, &meta, &by_id);
    if let Some(head) = input.head_sha {
        nudge_paused_review(client, repo, mcp, item, input.number, head, cfg, &meta);
    }
    state
}

/// Replies for `result` on `thread` unless the marker for `round` is already
/// there, then resolves the thread when the result is a fix. Reply before
/// resolve, always: a resolved thread with no reply reads as dismissed.
fn reply_and_settle(
    client: &Client,
    repo: &RepoId,
    number: u64,
    thread: &ReviewThread,
    round: u32,
    result: &ReviewResult,
) -> Result<(), crate::github::GitHubError> {
    let marker = ReplyMarker {
        thread: thread.id.clone(),
        round,
        sha: result.sha.clone().unwrap_or_default(),
        outcome: result.outcome,
    };
    let replied = already_replied(thread, round, result);
    match (result.outcome, replied) {
        (ReviewOutcome::Fixed, false) => {
            review_threads::reply_then_resolve(
                client,
                repo,
                number,
                thread,
                &render_reply(result, &marker),
            )?;
        }
        (ReviewOutcome::Fixed, true) => {
            // A restart between the reply and the resolve: finish the job
            // without repeating the reply.
            review_threads::resolve_review_thread(client, &thread.id)?;
        }
        (_, false) => {
            let root = thread
                .root()
                .ok_or_else(|| crate::github::GitHubError::Parse("thread has no root".into()))?;
            review_threads::reply_to_review_comment(
                client,
                repo,
                number,
                root.database_id,
                &render_reply(result, &marker),
            )?;
        }
        (_, true) => {}
    }
    Ok(())
}

/// Gates a thread that has been through `max_rounds` already: one item
/// comment, the needs-human gate label, the PR's stage label, one ping.
fn escalate_thread(
    mcp: &AgentflareMcp,
    input: &ReviewSweepInput<'_>,
    finding: &BotFinding,
    max_rounds: u32,
) {
    let item = input.item;
    let message = format!(
        "{REVIEW_THREAD_ESCALATED_MARKER}\n\nReview thread `{}` at `{}` ({}) is back for round {} \
         after {max_rounds} automatic round(s) — needs a human look before it loops further.\n\n\
         {}",
        finding.thread_id,
        finding.location(),
        finding.login,
        finding.round(),
        finding.summary_line()
    );
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(message.clone()),
        ..Default::default()
    });
    if let Some(gate_id) = input.label_id_by_name.get(NEEDS_HUMAN_GATE_LABEL) {
        let _ = mcp.item_add_label(ItemRequest {
            action: "add_label".into(),
            id: Some(item.id.clone()),
            label_id: Some(gate_id.clone()),
            ..Default::default()
        });
    }
    update_pr_stage(
        input.folder_path,
        input.number,
        stale_stage_label(input.labels),
        NEEDS_HUMAN_PR_LABEL,
        &message,
    );
    notify_human_gate(
        item,
        &format!(
            "review thread at {} is on round {} (> {max_rounds}) on PR #{}",
            finding.location(),
            finding.round(),
            input.number
        ),
    );
    update_metadata(mcp, &item.id, |m| {
        set_thread_escalated(m, &finding.thread_id)
    });
}

/// Results the agent reported under an id that is not a thread on the PR --
/// a finding from the review body, outside the diff -- get one PR-level
/// comment between them, never a thread reply.
fn post_detached_results(
    client: &Client,
    repo: &RepoId,
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    number: u64,
    meta: &serde_json::Map<String, serde_json::Value>,
    threads: &std::collections::HashMap<&str, &ReviewThread>,
) {
    let Some(results) = meta.get(REVIEW_RESULTS_KEY).and_then(|v| v.as_object()) else {
        return;
    };
    let detached: Vec<(String, ReviewResult)> = results
        .keys()
        .filter(|id| !threads.contains_key(id.as_str()))
        .filter_map(|id| thread_result(meta, id).map(|r| (id.clone(), r)))
        .collect();
    if detached.is_empty() {
        return;
    }
    let mut body = "## agentflare — review findings outside the diff\n".to_string();
    for (id, r) in &detached {
        let sha = r
            .sha
            .as_deref()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        body.push_str(&format!(
            "\n- `{id}`: {}{sha} — {}",
            r.outcome.as_str(),
            if r.note.trim().is_empty() {
                "no note"
            } else {
                r.note.trim()
            }
        ));
    }
    if let Err(e) = crate::github::issues::comment(client, repo, number, &body) {
        eprintln!(
            "agentflare-supervisor: could not post review-body results on PR #{number}: {}",
            e.log_safe()
        );
        return;
    }
    update_metadata(mcp, &item.id, |m| {
        for (id, _) in &detached {
            remove_thread_result(m, id);
        }
    });
}

/// The paused-review nudge for a PR the sweep sees as `Pending`: the bot's
/// own "Review paused" commit status is pending, so the PR reads as
/// pending CI and never reaches `sweep_review_threads` -- without this the
/// pause would hold the PR forever. Same soft-fail as `fetch_review_bot_state`.
pub(super) fn nudge_paused_review_for_pending(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
    number: u64,
    head: &str,
) {
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return;
    };
    let Ok(client) = Client::new() else {
        return;
    };
    let cfg = ReviewBotConfig::load(repo_root);
    let meta = current_metadata(mcp, item);
    nudge_paused_review(&client, &repo, mcp, item, number, head, &cfg, &meta);
}

/// A bot that auto-paused on a busy branch never reviews the new head on
/// its own: when its status says paused and it has not reviewed `head`,
/// summon it once for that head. The pause check itself runs once per head
/// (`REVIEW_NUDGED_HEAD_KEY` records the head last checked), so a PR that
/// isn't paused costs one status GET per push, not per tick.
#[allow(clippy::too_many_arguments)]
pub(super) fn nudge_paused_review(
    client: &Client,
    repo: &RepoId,
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    number: u64,
    head: &str,
    cfg: &ReviewBotConfig,
    meta: &serde_json::Map<String, serde_json::Value>,
) {
    if meta.get(REVIEW_NUDGED_HEAD_KEY).and_then(|v| v.as_str()) == Some(head) {
        return;
    }
    let Ok(statuses) = review_threads::list_commit_status_details(client, repo, head) else {
        return;
    };
    let mut done = true;
    if review_paused(&statuses, cfg) {
        let reviewed = crate::github::pulls::list_reviews(client, repo, number)
            .map(|reviews| bot_reviewed_head(&reviews, head, cfg))
            .unwrap_or(true);
        if !reviewed {
            let posted = crate::github::issues::list_comments(client, repo, number, None)
                .map(|comments| nudge_posted(&comments, head))
                .unwrap_or(true);
            if !posted
                && let Err(e) =
                    crate::github::issues::comment(client, repo, number, &render_nudge(cfg, head))
            {
                eprintln!(
                    "agentflare-supervisor: could not nudge the paused review on PR #{number}: {}",
                    e.log_safe()
                );
                done = false;
            }
        }
    }
    if done {
        update_metadata(mcp, &item.id, |m| {
            m.insert(REVIEW_NUDGED_HEAD_KEY.into(), head.into());
        });
    }
}

/// Swaps a stale `CODERABBIT_REPAIR_PR_LABEL` back to plain in-review once no
/// unresolved findings remain on the PR -- shared by `coderabbit_repair_or_gate`'s
/// own empty-findings branch and `merge_or_repair_findings`'s, so a PR
/// heading straight to `merge_if_approved` doesn't skip the same cleanup.
/// Only touches GitHub when the label is actually still there.
pub(super) fn clear_stale_coderabbit_repair_label(
    folder_path: &str,
    pr_number: u64,
    labels: &[String],
    summary: Option<&str>,
) {
    if labels.iter().any(|l| l == CODERABBIT_REPAIR_PR_LABEL) {
        let mut message =
            "## supervisor — CodeRabbit review clear\n\nNo unresolved findings remain.".to_string();
        if let Some(text) = summary.filter(|s| !s.trim().is_empty()) {
            message.push_str("\n\nRepair summary:\n");
            message.push_str(&text.chars().take(600).collect::<String>());
        }
        update_pr_stage(
            folder_path,
            pr_number,
            Some(CODERABBIT_REPAIR_PR_LABEL),
            IN_REVIEW_PR_LABEL,
            &message,
        );
    }
}

/// Dispatches a review-repair job for a CI-green PR whose bot review still
/// has unresolved findings, or -- once `quota::decide::SELF_REPAIR_CAP`
/// prior attempts have been made -- gates it for a human instead of
/// retrying forever. Mirrors `self_repair_or_gate` throughout -- same cap
/// accounting via a marker comment prefix, same claim/cooldown/host-pressure
/// gates before dispatching, same PR-stage-label convention -- so the two
/// dispatch paths can't quietly drift apart. `findings` is the sweep's
/// `ReviewBotState::to_dispatch`: each one's round is recorded on the item
/// once the job is queued, so the agent's `review_result` reports and the
/// next sweep's reply land on the right round.
#[allow(clippy::too_many_arguments)]
pub(super) fn coderabbit_repair_or_gate(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    auth_conn: &rusqlite::Connection,
    host_policy: agentflare_resource_gate::Policy,
    item: &agentflare_backend::item::Item,
    pr_number: u64,
    findings: &[BotFinding],
    labels: &[String],
    label_id_by_name: &std::collections::HashMap<String, String>,
    folder_path: &str,
) -> SelfRepairOutcome {
    if already_gated_or_in_flight(mcp, queue, item, label_id_by_name) {
        return SelfRepairOutcome::Skipped;
    }

    if findings.is_empty() {
        let summary = maybe_post_repair_complete_summary(
            mcp,
            item,
            CODERABBIT_REPAIR_MARKER,
            CODERABBIT_REPAIR_COMPLETE_MARKER,
            "All CodeRabbit findings are resolved",
            CODERABBIT_REPAIR_ANNOUNCED_KEY,
            CODERABBIT_REPAIR_SILENT_KEY,
            CODERABBIT_REPAIR_COMPLETED_KEY,
        );
        clear_stale_coderabbit_repair_label(folder_path, pr_number, labels, summary.as_deref());
        return SelfRepairOutcome::Skipped;
    }

    let (announced_for, silent_attempts, _) = repair_track(
        item,
        CODERABBIT_REPAIR_ANNOUNCED_KEY,
        CODERABBIT_REPAIR_SILENT_KEY,
        CODERABBIT_REPAIR_COMPLETED_KEY,
    );
    let fingerprint = coderabbit_findings_fingerprint(findings);
    let prior_markers = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
        .ok()
        .and_then(Result::ok)
        .map(|comments| {
            comments
                .iter()
                .filter(|c| c.body.starts_with(CODERABBIT_REPAIR_MARKER))
                .count() as u32
        })
        .unwrap_or(0);
    // Marker comments (announced dispatches) + the silent-dispatch counter =
    // every dispatch so far; retries without a new post still count.
    let prior_attempts = prior_markers + silent_attempts;

    if prior_attempts >= crate::quota::decide::SELF_REPAIR_CAP {
        let cap_message = format!(
            "## supervisor — CodeRabbit review repair cap reached\n\n{} unresolved finding(s) \
             remain. {} automatic repair attempt(s) already made — needs a human look.",
            findings.len(),
            crate::quota::decide::SELF_REPAIR_CAP,
        );
        let _ = mcp.comment_impl(CommentRequest {
            action: "create".into(),
            item_id: Some(item.id.clone()),
            body: Some(cap_message.clone()),
            ..Default::default()
        });
        if let Some(gate_id) = label_id_by_name.get(NEEDS_HUMAN_GATE_LABEL) {
            let _ = mcp.item_add_label(ItemRequest {
                action: "add_label".into(),
                id: Some(item.id.clone()),
                label_id: Some(gate_id.clone()),
                ..Default::default()
            });
        }
        update_pr_stage(
            folder_path,
            pr_number,
            Some(CODERABBIT_REPAIR_PR_LABEL),
            NEEDS_HUMAN_PR_LABEL,
            &cap_message,
        );
        notify_human_gate(
            item,
            &format!(
                "CodeRabbit review repair cap reached ({} attempt(s), {} finding(s) still \
                 unresolved)",
                crate::quota::decide::SELF_REPAIR_CAP,
                findings.len()
            ),
        );
        return SelfRepairOutcome::Skipped;
    }

    // Same item #114 rationale as `self_repair_or_gate`: dispatching while
    // the item's own claim is still live would just die instantly at
    // `execute_work`'s claim-acquire step.
    let claim_still_live = mcp
        .with_backend_db(|conn| {
            let requested_ttl = crate::mcp_server::types::backend_claim_ttl_secs();
            let ttl = agentflare_backend::claim::effective_ttl_secs(conn, &item.id, requested_ttl);
            agentflare_backend::claim::has_active_claim_by_other(
                conn,
                &item.id,
                "",
                crate::claims::now(),
                ttl,
            )
        })
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false);
    if claim_still_live {
        return SelfRepairOutcome::Deferred;
    }

    // Same item #261 rationale as `self_repair_or_gate`: arbitrate across
    // workstations via a claim marker comment on the PR itself, since a
    // per-workstation backend db can't see another workstation's dispatch.
    // Same soft-fail as `self_repair_or_gate`'s own resolution here: no
    // remote/no credentials proceeds rather than blocking dispatch on it.
    let repo_and_client =
        RepoId::resolve_from_remote(std::path::Path::new(folder_path)).zip(Client::new().ok());
    if let Some((repo, client)) = repo_and_client
        && !claim_self_repair(&client, &repo, pr_number, &item.id, crate::claims::now())
    {
        return SelfRepairOutcome::Deferred;
    }

    let Some(agent) = item
        .assignee_agent
        .as_deref()
        .and_then(resolve_confirmed_agent)
        .or_else(|| route_unassigned(item))
    else {
        return SelfRepairOutcome::Skipped;
    };
    if crate::auth_db::is_cooling_down(auth_conn, agent.as_str()) {
        return SelfRepairOutcome::Deferred;
    }
    if host_policy.blocks_dispatch() {
        return SelfRepairOutcome::Deferred;
    }

    let summary: Vec<String> = findings
        .iter()
        .take(10)
        .map(BotFinding::summary_line)
        .collect();
    let overflow = findings.len().saturating_sub(10);
    let overflow_line = if overflow > 0 {
        format!("\n- …and {overflow} more")
    } else {
        String::new()
    };

    let reason = format!(
        "CodeRabbit review: {} unresolved finding(s)",
        findings.len()
    );
    let Some(info) = enqueue_work_job(queue, item, agent, Some(folder_path), Some(&reason)) else {
        return SelfRepairOutcome::Skipped;
    };
    // The round each thread is now on, so `review_result` reports and the
    // next sweep's replies line up with this dispatch.
    update_metadata(mcp, &item.id, |m| {
        for f in findings {
            set_thread_round(m, &f.thread_id, f.round());
        }
    });
    if announced_for.as_deref() == Some(fingerprint.as_str()) {
        // Same findings already announced — re-dispatch the retry silently
        // instead of posting the identical announcement again.
        persist_repair_track(
            mcp,
            &item.id,
            CODERABBIT_REPAIR_ANNOUNCED_KEY,
            CODERABBIT_REPAIR_SILENT_KEY,
            CODERABBIT_REPAIR_COMPLETED_KEY,
            None,
            true,
            None,
        );
        // The PR stage label may have been reverted out-of-band; ensure it
        // without commenting (empty comment = label bookkeeping only).
        if !labels.iter().any(|l| l == CODERABBIT_REPAIR_PR_LABEL) {
            update_pr_stage(
                folder_path,
                pr_number,
                stale_stage_label(labels),
                CODERABBIT_REPAIR_PR_LABEL,
                "",
            );
        }
        return SelfRepairOutcome::Dispatched;
    }
    persist_repair_track(
        mcp,
        &item.id,
        CODERABBIT_REPAIR_ANNOUNCED_KEY,
        CODERABBIT_REPAIR_SILENT_KEY,
        CODERABBIT_REPAIR_COMPLETED_KEY,
        Some(&fingerprint),
        false,
        None,
    );
    let max_rounds = ReviewBotConfig::load(std::path::Path::new(folder_path)).max_rounds;
    let task = render_fix_task(
        &findings.iter().collect::<Vec<_>>(),
        pr_number,
        &item.id,
        max_rounds,
    );
    // The PR gets the summary; the full task (finding bodies included) is
    // for the agent, on the item.
    let pr_message = format!(
        "{CODERABBIT_REPAIR_MARKER}\n\nThe review bot left {} unresolved finding(s) on this PR:\n\n\
         {}{overflow_line}\n\njob: {}",
        findings.len(),
        summary.join("\n"),
        info.id,
    );
    let dispatch_message = format!(
        "{CODERABBIT_REPAIR_MARKER}\n\nThe review bot left {} unresolved finding(s) on this PR:\n\n\
         {}{overflow_line}\n\n{task}\n\njob: {}",
        findings.len(),
        summary.join("\n"),
        info.id,
    );
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(dispatch_message),
        ..Default::default()
    });
    update_pr_stage(
        folder_path,
        pr_number,
        Some(IN_REVIEW_PR_LABEL),
        CODERABBIT_REPAIR_PR_LABEL,
        &pr_message,
    );
    SelfRepairOutcome::Dispatched
}
