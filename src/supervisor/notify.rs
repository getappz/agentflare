//! Outbound half of the supervisor's Telegram notifications: the pings and
//! approval cards it sends when an item lands on a gate. The inbound half
//! (polling `getUpdates` and acting on a tapped button or chat message)
//! lives in the sibling `telegram` module -- split there first (master, PR
//! #677) for the chat channel, before item #573's plan-gate work pushed
//! `supervisor.rs` past the 1500-line LOC gate (`scripts/loc-gate.sh`) and
//! moved these outbound pieces out too. Re-exported by the parent as
//! `pub(crate) use notify::*`, so existing `crate::supervisor::…` paths --
//! and `supervisor_tests.rs`'s `use super::*` -- are unchanged.
//!
//! Everything here is fail-open by contract: each function no-ops when
//! `TELEGRAM_NOTIFY_CHAT_ID_SECRET` isn't configured (notifications are
//! opt-in), and a configured-but-failing send only logs. A notification
//! failure must never block the gate it is reporting. That fail-open
//! contract extends to "running under `cargo test`" too -- see
//! `test_notify_disabled` below -- since a fake, in-memory-only test item
//! has no business reaching a developer's real vault or Telegram chat
//! (item #274).

use super::*;

/// Test-only circuit breaker for both fail-open entry points below
/// (`notify_item`, `notify_hourly_digest`). `cargo test --bin
/// agentflare` compiles this whole crate with `cfg(test)` active -- not
/// just code inside `#[cfg(test)] mod` blocks -- so without this, ANY test
/// that reaches a human-gate or plan/PR-approval code path reads whatever
/// vault + chat id happen to be configured on the machine running the
/// suite and can fire a real Telegram message referencing a fake item id
/// that exists only in that test's `:memory:` backend. Tests isolate
/// `$HOME` via `paths::test_support::with_temp_home` when they need to
/// exercise the vault lookup itself (e.g. asserting on the "no token
/// configured" error path); everything else just wants this to no-op,
/// which it now does regardless of whether a given test remembered to
/// isolate `$HOME`. Compiled out of release builds entirely, so production
/// behavior is unaffected.
#[cfg(test)]
fn test_notify_disabled() -> bool {
    std::env::var("AGENTFLARE_TEST_ALLOW_NOTIFY").as_deref() != Ok("1")
}

#[cfg(not(test))]
fn test_notify_disabled() -> bool {
    false
}

/// Cap on `notify_human_gate`'s summarized reason line. Some callers (e.g.
/// `cli::work_auth_expiry`'s `handle_auth_expired`) pass a `reason` that
/// embeds a dispatch failure's full message, which can itself carry
/// `agent_launch::diagnostic_suffix`'s multi-KB raw stdout/stderr/sandbox-log
/// tail (item #269) -- fine in the item comment thread, unreadable as a
/// phone push notification.
const NOTIFY_REASON_MAX_CHARS: usize = 240;

/// Reduces a possibly multi-line, possibly huge failure `reason` to the last
/// non-blank line (where the actual error typically ends up, after any raw
/// output dumped above it), capped to `NOTIFY_REASON_MAX_CHARS`.
fn summarize_reason(reason: &str) -> String {
    let last_line = reason
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(reason)
        .trim();
    match last_line.char_indices().nth(NOTIFY_REASON_MAX_CHARS) {
        Some((cut, _)) => format!("{}…", &last_line[..cut]),
        None => last_line.to_string(),
    }
}

/// Escape the characters Telegram's HTML `parse_mode` treats specially, so
/// an arbitrary item title/description can't break card formatting (or be
/// interpreted as an unintended tag).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Best-effort Telegram ping for an item that just landed on a human gate
/// (a go/no-go decision, an unanswerable question, or a CI self-repair cap).
/// Same HTML-card formatting as [`notify_pr_approval_gate`] and
/// [`notify_plan_approval_gate`], just without a button -- none of this
/// function's callers have a single-tap follow-up action to attach one to
/// (unlike the PR/plan gates, which do). Silently does nothing when
/// `TELEGRAM_NOTIFY_CHAT_ID_SECRET` isn't configured, since notifications
/// are opt-in and a bare install shouldn't spam stderr every tick; a
/// configured-but-failing send only logs -- a notification failure must
/// never block the gate itself.
pub(crate) fn notify_human_gate(item: &agentflare_backend::item::Item, reason: &str) {
    let reason = summarize_reason(reason);
    let text = format!(
        "\u{1F514} <b>agentflare</b> needs a human\n\
         <b>Item:</b> #{} \u{2014} {}\n\
         {}",
        item.sequence_id,
        html_escape(&item.name),
        html_escape(&reason),
    );
    notify_item(
        Some(item),
        &format!("human-gate:{reason}"),
        NotificationCard {
            prepare: None,
            text: &text,
            reason: &reason,
            button: None,
            attachment: None,
        },
    );
}

/// Every item/host notification, including approval attachments, passes here.
struct NotificationCard<'a> {
    text: &'a str,
    reason: &'a str,
    button: Option<(&'a str, &'a str)>,
    attachment: Option<(&'a str, &'a [u8])>,
    prepare: Option<&'a dyn Fn() -> String>,
}

fn deliver_notification(
    store: &agentflare_store::Store,
    item: Option<&agentflare_backend::item::Item>,
    event_key: &str,
    card: &NotificationCard<'_>,
    send: impl FnOnce(&NotificationCard<'_>),
) -> Result<(), agentflare_store::Error> {
    let (id, seq, name) = item.map_or(("host", 0, "host"), |i| {
        (i.id.as_str(), i.sequence_id, i.name.as_str())
    });
    if agentflare_store::notify_ledger::decide_and_record(
        store,
        id,
        event_key,
        card.reason,
        seq,
        name,
        chrono::Utc::now().timestamp(),
    )? == agentflare_store::notify_ledger::NotifyDecision::Immediate
    {
        send(card);
    }
    Ok(())
}

fn notify_item(
    item: Option<&agentflare_backend::item::Item>,
    event_key: &str,
    card: NotificationCard<'_>,
) {
    #[cfg(test)]
    if FAKE_NOTIFICATIONS.with(|fake| {
        let mut fake = fake.borrow_mut();
        if let Some((store, sent)) = fake.as_mut() {
            deliver_notification(store, item, event_key, &card, |card| {
                sent.push(card.text.to_string())
            })
            .unwrap();
            true
        } else {
            false
        }
    }) {
        return;
    }
    if test_notify_disabled() {
        return;
    }
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    // Failure to open/claim the ledger suppresses the send, never the gate.
    let result = crate::store::open().and_then(|store| {
        deliver_notification(&store, item, event_key, &card, |card| {
            let seq = item.map_or(0, |i| i.sequence_id);
            send_notification_card(&chat_id, card, seq, event_key);
        })
    });
    if let Err(e) = result {
        eprintln!("agentflare-supervisor: telegram ledger failed for event {event_key}: {e}");
    }
}

/// Share the chat-turn lock with conversational replies. Log each actual send
/// with its stable event identity so daemon logs reveal repeat sources.
fn send_notification_card(chat_id: &str, card: &NotificationCard<'_>, seq: i64, event: &str) {
    let turn_lock = crate::chat_channel::chat_turn_lock(chat_id);
    let _turn_guard = turn_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((filename, bytes)) = card.attachment {
        eprintln!("agentflare-supervisor: telegram document send item #{seq} event={event}");
        if let Err(e) = crate::channels::send_telegram_document(chat_id, filename, bytes) {
            eprintln!("agentflare-supervisor: telegram document notify failed: {e}");
        }
    }
    let buttons: Vec<_> = card.button.into_iter().collect();
    eprintln!("agentflare-supervisor: telegram card send item #{seq} event={event}");
    let prepared = card.prepare.map(|prepare| prepare());
    let text = prepared.as_deref().unwrap_or(card.text);
    if let Err(e) = crate::channels::send_telegram_card(chat_id, text, &buttons) {
        eprintln!(
            "agentflare-supervisor: telegram notify failed for item #{seq} event={event}: {e}"
        );
    }
}

#[cfg(test)]
thread_local! {
    static FAKE_NOTIFICATIONS: std::cell::RefCell<Option<(agentflare_store::Store, Vec<String>)>> = const { std::cell::RefCell::new(None) };
}

/// Thread-local injection keeps tests off the real vault and Telegram without
/// changing process-wide environment variables or bypassing the ledger.
#[cfg(test)]
pub(crate) fn with_fake_notifications<R>(run: impl FnOnce() -> R) -> (R, Vec<String>) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FAKE_NOTIFICATIONS.with(|f| *f.borrow_mut() = None);
        }
    }
    FAKE_NOTIFICATIONS.with(|f| {
        *f.borrow_mut() = Some((agentflare_store::Store::open_memory().unwrap(), Vec::new()))
    });
    let _reset = Reset;
    let result = run();
    let sent = FAKE_NOTIFICATIONS.with(|f| f.borrow_mut().take().unwrap().1);
    (result, sent)
}

/// FYI notification for a quota/failover event (nobody has to act): same
/// opt-in and fail-open contract as [`notify_human_gate`], but `text` is sent
/// as-is under a neutral headline. Subsequent events join the hourly digest.
pub(crate) fn notify_quota_event(item: &agentflare_backend::item::Item, text: &str) {
    let card = format!(
        "\u{2139}\u{FE0F} <b>agentflare</b> quota\n<b>Item:</b> #{} \u{2014} {}\n{}",
        item.sequence_id,
        html_escape(&item.name),
        html_escape(&summarize_reason(text)),
    );
    notify_item(
        Some(item),
        &format!("quota:{}", summarize_reason(text)),
        NotificationCard {
            prepare: None,
            text: &card,
            reason: &summarize_reason(text),
            button: None,
            attachment: None,
        },
    );
}

/// Once-per-tick view of the host resource gate: logs a single summary line
/// while it holds work, and sends a quota-style FYI only when the gate
/// *transitions* (open -> held, held -> open), never per item per tick.
pub(crate) fn report_gate_state(policy: agentflare_resource_gate::Policy, held: usize) {
    use std::sync::atomic::AtomicBool;
    static WAS_BLOCKED: AtomicBool = AtomicBool::new(false);
    let blocked = policy.blocks_dispatch();
    if blocked && held > 0 {
        eprintln!(
            "agentflare-supervisor: host resource gate is {}; {held} ready-for-work item(s) held",
            policy.as_str()
        );
    }
    if !gate_flipped(&WAS_BLOCKED, blocked, held) || test_notify_disabled() {
        return;
    }
    let text = if blocked {
        let why = policy
            .pause_reason()
            .map_or(String::new(), |r| format!(" ({})", r.as_str()));
        let cpu = agentflare_resource_gate::Signals::sample().cpu_usage_pct;
        format!(
            "host resource gate {}{why}: cpu {cpu:.0}%, {held} ready-for-work item(s) held",
            policy.as_str()
        )
    } else {
        "host resource gate resumed: dispatch is flowing again".to_string()
    };
    let card = format!(
        "\u{2139}\u{FE0F} <b>agentflare</b> host\n{}",
        html_escape(&text)
    );
    notify_item(
        None,
        &format!("host-gate:{}", policy.as_str()),
        NotificationCard {
            prepare: None,
            text: &card,
            reason: &text,
            button: None,
            attachment: None,
        },
    );
}

/// Whether the gate just changed between holding work and flowing. A gate
/// that is closed with nothing waiting is not "held": it neither announces
/// itself nor arms the later "resumed" notice.
fn gate_flipped(was_blocked: &std::sync::atomic::AtomicBool, blocked: bool, held: usize) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    if blocked && held == 0 {
        return false;
    }
    was_blocked.swap(blocked, Relaxed) != blocked
}

/// What is holding a CI-green PR back from merging -- decides the wording of
/// [`notify_pr_approval_gate`]'s card, so a human reading it knows which
/// action actually unblocks the PR instead of being told to attach a label
/// that GitHub would still ignore.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrApprovalBlocker {
    /// GitHub would merge it; only agentflare's own `PR_APPROVAL_LABEL` is
    /// missing. One tap on the card's button attaches it.
    ApprovalLabel,
    /// Branch protection is holding it for a human review
    /// (`PrCiStatus::AwaitingReview`): an approving review is required, or
    /// a reviewer requested changes. The label alone can't merge this.
    GitHubReview {
        changes_requested: bool,
        /// Whether `PR_APPROVAL_LABEL` is still missing too, so the card can
        /// say the review is the first of two steps and keep its button.
        label_missing: bool,
    },
}

/// Side-by-side diff excerpt for the approval card; fail-open on GitHub errors.
fn pr_approval_diff_context(repo: &crate::github::RepoId, number: u64) -> Option<String> {
    let client = crate::github::Client::new().ok()?;
    let files = crate::github::pulls::list_files(&client, repo, number).ok()?;
    crate::review::diff_display::first_change_summary(&files)
}

/// The HTML card body for [`notify_pr_approval_gate`], split out so its
/// wording is testable without a Telegram chat.
fn pr_approval_card_text(
    item: &agentflare_backend::item::Item,
    repo: &crate::github::RepoId,
    number: u64,
    blocker: PrApprovalBlocker,
    diff_context: Option<&str>,
) -> String {
    let excerpt: String = item.description.chars().take(200).collect();
    let pr_link = format!("PR <a href=\"https://github.com/{repo}/pull/{number}\">#{number}</a>");
    let status = match blocker {
        PrApprovalBlocker::ApprovalLabel => format!(
            "{pr_link} is CI-green and mergeable, awaiting <code>{PR_APPROVAL_LABEL}</code>."
        ),
        PrApprovalBlocker::GitHubReview {
            changes_requested,
            label_missing,
        } => {
            let why = if changes_requested {
                "a reviewer requested changes"
            } else {
                "branch protection requires an approving review"
            };
            let then = if label_missing {
                format!(" It also still needs <code>{PR_APPROVAL_LABEL}</code> to auto-merge.")
            } else {
                String::new()
            };
            format!(
                "{pr_link} is CI-green, but GitHub is blocking the merge until a human review \
                 lands: {why}. \
                 <a href=\"https://github.com/{repo}/pull/{number}/files\">Review it here</a>.{then}"
            )
        }
    };
    let diff_block = diff_context
        .filter(|s| !s.is_empty())
        .map(|ctx| {
            format!(
                "\n<b>Changes:</b>\n<pre>{ctx}</pre>\n",
                ctx = html_escape(ctx)
            )
        })
        .unwrap_or_default();
    format!(
        "\u{1F514} <b>agentflare</b> needs a human\n\
         <b>Repo:</b> {repo}\n\
         <b>Item:</b> #{} \u{2014} {}\n\
         {}{}\n\n\
         {status}",
        item.sequence_id,
        html_escape(&item.name),
        html_escape(&excerpt),
        diff_block,
    )
}

/// Telegram-only rich variant of [`notify_human_gate`] for a CI-green PR
/// waiting on a human. When `PR_APPROVAL_LABEL` is all that is missing
/// (`PrApprovalBlocker::ApprovalLabel`) the human can resolve it with a
/// single tap: the card carries an inline "Approve" button whose
/// `callback_data` embeds the repo and PR number directly
/// (`approve:{owner}/{repo}#{number}`) -- self-contained, so
/// [`poll_telegram_approvals`] never needs to re-resolve a worktree path to
/// act on a click. When GitHub itself is holding the PR for a review
/// (`PrApprovalBlocker::GitHubReview`) the card says so and links the
/// review instead of asking for a label that can't merge it; the button is
/// kept only while the label is missing as well. Same fail-open contract as
/// `notify_human_gate`: no-ops without a configured chat id or a resolvable
/// repo, and a send failure only logs.
pub(crate) fn notify_pr_approval_gate(
    item: &agentflare_backend::item::Item,
    folder_path: &str,
    number: u64,
    blocker: PrApprovalBlocker,
    head_sha: Option<&str>,
) {
    if test_notify_disabled() {
        return;
    }
    let Some(repo) = crate::github::RepoId::resolve_from_remote(std::path::Path::new(folder_path))
    else {
        return;
    };
    let text = pr_approval_card_text(item, &repo, number, blocker, None);
    let prepare = || {
        let diff_context = pr_approval_diff_context(&repo, number);
        pr_approval_card_text(item, &repo, number, blocker, diff_context.as_deref())
    };
    let wants_label = !matches!(
        blocker,
        PrApprovalBlocker::GitHubReview {
            label_missing: false,
            ..
        }
    );
    let callback = format!("approve:{repo}#{number}");
    let reason = format!("awaiting PR approval/review: https://github.com/{repo}/pull/{number}");
    notify_item(
        Some(item),
        &format!(
            "pr-approval:{repo}:{number}:{}:{blocker:?}",
            head_sha.unwrap_or("unknown")
        ),
        NotificationCard {
            prepare: Some(&prepare),
            text: &text,
            reason: &reason,
            button: wants_label.then_some(("\u{2705} Approve", callback.as_str())),
            attachment: None,
        },
    );
}

/// Human-approval card for a submitted plan -- the plan-gate sibling of
/// `notify_pr_approval_gate`. `plan_asset_id` is included in the card text
/// (not the callback data, which stays a plain item reference) purely so
/// a human reading the notification knows which asset to open. Same
/// fail-open contract as `notify_human_gate`: no-ops without a configured
/// chat id, and a send failure only logs. `plan_file` (filename, bytes) is
/// sent as a document just before the card so the plan can be read in-chat.
pub(crate) fn notify_plan_approval_gate(
    item: &agentflare_backend::item::Item,
    plan_asset_id: &str,
    plan_file: Option<(String, Vec<u8>)>,
) {
    let text = format!(
        "\u{1F4CB} <b>agentflare</b> plan needs review\n\
         <b>Item:</b> #{} \u{2014} {}\n\
         Plan asset: <code>{}</code>",
        item.sequence_id,
        html_escape(&item.name),
        html_escape(plan_asset_id),
    );
    let callback = format!("approve_plan:{}", item.id);
    notify_item(
        Some(item),
        &format!("plan-approval:{plan_asset_id}"),
        NotificationCard {
            prepare: None,
            text: &text,
            reason: &format!("plan needs review: {plan_asset_id}"),
            button: Some(("\u{2705} Approve", callback.as_str())),
            attachment: plan_file.as_ref().map(|(n, b)| (n.as_str(), b.as_slice())),
        },
    );
}

const DIGEST_HEADING: &str = "agentflare — hourly attention summary";

fn digest_needs_document(text: &str) -> bool {
    DIGEST_HEADING.encode_utf16().count() + 1 + text.encode_utf16().count()
        > flare_channels::telegram::TELEGRAM_MAX_MESSAGE_LENGTH
}

/// Called from the daemon tick, with a durable rolling-hour reservation.
/// Query current gates only when due; repeated ticks do no GitHub work here.
pub(crate) fn notify_hourly_digest(mcp: &AgentflareMcp) {
    if test_notify_disabled() {
        return;
    }
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    let result = (|| -> Result<(), agentflare_store::Error> {
        let store = crate::store::open()?;
        let now = chrono::Utc::now().timestamp();
        if agentflare_store::notify_ledger::last_digest_at(&store)?
            .is_some_and(|last| now.saturating_sub(last) < 3600)
        {
            return Ok(());
        }
        let current = current_attention(mcp, &store);
        agentflare_store::notify_ledger::send_digest(&store, &current, now, |text| {
            let turn_lock = crate::chat_channel::chat_turn_lock(&chat_id);
            let _turn_guard = turn_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let result = if digest_needs_document(text) {
                eprintln!(
                    "agentflare-supervisor: telegram document send item #0 event=hourly-digest"
                );
                crate::channels::send_telegram_document(
                    &chat_id,
                    "hourly-attention-summary.txt",
                    format!("{DIGEST_HEADING}\n{text}").as_bytes(),
                )
            } else {
                eprintln!("agentflare-supervisor: telegram card send item #0 event=hourly-digest");
                crate::channels::send_telegram_card(
                    &chat_id,
                    &format!("<b>{DIGEST_HEADING}</b>\n{}", html_escape(text)),
                    &[],
                )
            };
            match result {
                Ok(_) => true,
                Err(e) => {
                    eprintln!("agentflare-supervisor: telegram digest failed: {e}");
                    false
                }
            }
        })?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("agentflare-supervisor: telegram digest ledger failed: {e}");
    }
}

/// Human gates can outlive their last notification call. Include them on
/// every hourly snapshot, using the retained latest reason when available.
fn current_attention(
    mcp: &AgentflareMcp,
    store: &agentflare_store::Store,
) -> Vec<agentflare_store::notify_ledger::DigestEntry> {
    let candidates = mcp
        .with_backend_db(|conn| {
            let mut candidates = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for dir in agentflare_backend::project_dir::list(conn).unwrap_or_default() {
                if !seen.insert(dir.project_id.clone()) {
                    continue;
                }
                let labels = agentflare_backend::label::list_by_project(conn, &dir.project_id)
                    .unwrap_or_default();
                for item in agentflare_backend::item::list_by_project(conn, &dir.project_id)
                    .unwrap_or_default()
                {
                    if item.archived_at.is_some() {
                        continue;
                    }
                    let group = agentflare_backend::state::get(conn, &item.state_id)
                        .map(|s| s.group_name)
                        .unwrap_or_default();
                    if matches!(group.as_str(), "completed" | "cancelled") {
                        continue;
                    }
                    let ids =
                        agentflare_backend::item::list_labels(conn, &item.id).unwrap_or_default();
                    let names: Vec<_> = labels
                        .iter()
                        .filter(|l| ids.contains(&l.id))
                        .map(|l| l.name.as_str())
                        .collect();
                    let human_gate = names.iter().find(|name| {
                        matches!(
                            **name,
                            NEEDS_HUMAN_GATE_LABEL | NEEDS_DECISION_LABEL | NEEDS_MANUAL_LABEL
                        )
                    });
                    let plan = agentflare_backend::item::plan_gate::read_plan_gate(&item.metadata);
                    let plan_blocked = matches!(
                        agentflare_backend::item::plan_gate::plan_gate_status(&item.metadata),
                        agentflare_backend::item::PlanGateStatus::Blocked(_)
                    );
                    let reason = if let Some(gate) = human_gate {
                        Some(format!("awaiting human: {gate}"))
                    } else if group == "needs_input" {
                        Some("awaiting human input".to_string())
                    } else if plan.plan_status.as_deref() == Some("pending")
                        || (plan_blocked && names.contains(&READY_LABEL))
                    {
                        Some("awaiting plan approval".to_string())
                    } else {
                        None
                    };
                    if reason.is_some() || group == "in_review" {
                        candidates.push((item, dir.folder_path.clone(), reason));
                    }
                }
            }
            candidates
        })
        .unwrap_or_default();
    candidates
        .into_iter()
        .filter_map(|(item, folder, mut reason)| {
            // Reuse the live CI/review interpretation used by the review sweep,
            // so approved, merged or closed PRs don't remain attention items.
            if reason.is_none() {
                reason = match crate::worktree::pr_ci_status(&item, std::path::Path::new(&folder)) {
                    crate::worktree::PrCiStatus::AwaitingReview { number, .. } => Some(format!(
                        "awaiting PR review: {}",
                        approval_pr_url(&folder, number)
                    )),
                    crate::worktree::PrCiStatus::Passing { number, labels, .. }
                        if !labels.iter().any(|l| l == PR_APPROVAL_LABEL) =>
                    {
                        Some(format!(
                            "awaiting PR approval: {}",
                            approval_pr_url(&folder, number)
                        ))
                    }
                    _ => None,
                };
            }
            let fallback = reason?;
            let link = crate::worktree::pr_number_from_metadata(&item)
                .map(|number| approval_pr_url(&folder, number))
                .or_else(|| {
                    fallback
                        .split_whitespace()
                        .find(|word| word.starts_with("https://github.com/"))
                        .map(str::to_string)
                });
            let (latest_text, updated_at) =
                agentflare_store::notify_ledger::latest_reason(store, &item.id)
                    .ok()
                    .flatten()
                    .unwrap_or((fallback, item.updated_at));
            Some(agentflare_store::notify_ledger::DigestEntry {
                item_id: item.id,
                event_key: "current-gate".into(),
                latest_text,
                seq: item.sequence_id,
                name: item.name,
                updated_at,
                revision: 0,
                link,
            })
        })
        .collect()
}

fn approval_pr_url(folder: &str, number: u64) -> String {
    crate::github::RepoId::resolve_from_remote(std::path::Path::new(folder)).map_or_else(
        || format!("PR #{number}"),
        |repo| format!("https://github.com/{repo}/pull/{number}"),
    )
}

/// Parse an "Approve" button's `callback_data` (`approve_plan:{item_id}`)
/// built by [`notify_plan_approval_gate`]. The inbound half lives in
/// `supervisor::telegram::handle_telegram_callback`, alongside
/// `parse_approve_callback`'s PR-approval handling -- both callback flavors
/// share the one Telegram poller, so they share the one dispatch point.
pub(crate) fn parse_plan_approve_callback(data: &str) -> Option<String> {
    data.strip_prefix("approve_plan:").map(str::to_string)
}

/// Process-local suppression for repeated comments/stage updates. Telegram
/// delivery always enforces the independent durable ledger above.
pub(crate) fn first_time_gated(item_id: &str) -> bool {
    static NOTIFIED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    NOTIFIED
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(item_id.to_string())
}

#[cfg(test)]
mod pr_approval_card_tests {
    use super::*;

    fn item() -> agentflare_backend::item::Item {
        agentflare_backend::item::Item {
            id: "item-12".into(),
            project_id: "p".into(),
            state_id: "s".into(),
            name: "Widen the <gate>".into(),
            description: "desc".into(),
            priority: "none".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id: 12,
            sort_order: 0.0,
            started_at: None,
            completed_at: None,
            archived_at: None,
            external_source: None,
            external_id: None,
            metadata: "{}".into(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
            start_date: None,
            due_date: None,
        }
    }

    fn repo() -> crate::github::RepoId {
        crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    #[test]
    fn delivery_deduplicates_events_and_preserves_first_buttons_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let item = item();
        let mut sent = Vec::new();
        for event in [
            "plan-approval:asset",
            "plan-approval:asset",
            "pr-approval:7:sha",
        ] {
            let store = agentflare_store::Store::open_file(&path).unwrap();
            let card = NotificationCard {
                prepare: None,
                text: "approval card",
                reason: event,
                button: Some(("Approve", "callback")),
                attachment: Some(("plan.md", b"plan")),
            };
            deliver_notification(&store, Some(&item), event, &card, |card| {
                assert_eq!(card.button, Some(("Approve", "callback")));
                assert_eq!(card.attachment, Some(("plan.md", b"plan".as_slice())));
                sent.push(card.text.to_string());
            })
            .unwrap();
        }
        assert_eq!(sent, ["approval card"]);
        let store = agentflare_store::Store::open_file(&path).unwrap();
        assert_eq!(
            agentflare_store::notify_ledger::list_digest(&store)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn human_quota_plan_and_host_share_the_durable_policy() {
        let item = item();
        let (_, sent) = with_fake_notifications(|| {
            notify_human_gate(&item, "first");
            notify_human_gate(&item, "first");
            notify_quota_event(&item, "quota changed");
            notify_plan_approval_gate(&item, "asset", Some(("plan.md".into(), b"plan".to_vec())));
            let card = || NotificationCard {
                prepare: None,
                text: "host",
                reason: "held",
                button: None,
                attachment: None,
            };
            notify_item(None, "host-gate:held", card());
            notify_item(None, "host-gate:resumed", card());
        });
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn oversized_digest_uses_one_document_instead_of_rejected_message() {
        let room = 4096 - DIGEST_HEADING.encode_utf16().count() - 1;
        assert!(!digest_needs_document(&"x".repeat(room)));
        assert!(digest_needs_document(&"x".repeat(room + 1)));
        assert!(digest_needs_document(&"😀".repeat(room / 2 + 1)));
    }

    #[test]
    fn repeated_delivery_does_not_prepare_rich_card() {
        let store = agentflare_store::Store::open_memory().unwrap();
        let item = item();
        let prepared = std::cell::Cell::new(0);
        let prepare = || {
            prepared.set(prepared.get() + 1);
            "rich".to_string()
        };
        let card = NotificationCard {
            text: "base",
            reason: "reason",
            button: None,
            attachment: None,
            prepare: Some(&prepare),
        };
        for _ in 0..3 {
            deliver_notification(&store, Some(&item), "approval", &card, |card| {
                assert_eq!((card.prepare.unwrap())(), "rich");
            })
            .unwrap();
        }
        assert_eq!(prepared.get(), 1);
    }

    #[test]
    fn label_blocker_asks_for_the_approval_label() {
        let text =
            pr_approval_card_text(&item(), &repo(), 7, PrApprovalBlocker::ApprovalLabel, None);
        assert!(
            text.contains("awaiting <code>status:pr:approved</code>"),
            "{text}"
        );
        assert!(
            text.contains("Widen the &lt;gate&gt;"),
            "escaped title: {text}"
        );
        assert!(!text.contains("Review it here"), "{text}");
    }

    #[test]
    fn review_blocker_names_the_github_review_and_links_it() {
        let text = pr_approval_card_text(
            &item(),
            &repo(),
            7,
            PrApprovalBlocker::GitHubReview {
                changes_requested: false,
                label_missing: false,
            },
            None,
        );
        assert!(
            text.contains("branch protection requires an approving review"),
            "{text}"
        );
        assert!(
            text.contains("href=\"https://github.com/o/r/pull/7/files\">Review it here</a>"),
            "{text}"
        );
        assert!(
            !text.contains("awaiting <code>status:pr:approved</code>"),
            "a review-blocked PR must not be described as waiting on the label: {text}"
        );
        assert!(!text.contains("also still needs"), "{text}");
    }

    #[test]
    fn review_blocker_says_changes_were_requested_and_mentions_a_missing_label() {
        let text = pr_approval_card_text(
            &item(),
            &repo(),
            7,
            PrApprovalBlocker::GitHubReview {
                changes_requested: true,
                label_missing: true,
            },
            None,
        );
        assert!(text.contains("a reviewer requested changes"), "{text}");
        assert!(
            text.contains("also still needs <code>status:pr:approved</code>"),
            "{text}"
        );
    }
}

#[cfg(test)]
mod summarize_reason_tests {
    use super::*;

    #[test]
    fn gate_notifies_once_per_transition_and_only_when_work_is_held() {
        use std::sync::atomic::AtomicBool;
        let f = AtomicBool::new(false);
        assert!(!gate_flipped(&f, true, 0), "closed with nothing held");
        assert!(!gate_flipped(&f, false, 0), "still flowing");
        assert!(gate_flipped(&f, true, 3), "now holding work");
        assert!(!gate_flipped(&f, true, 5), "same state next tick");
        assert!(gate_flipped(&f, false, 0), "resumed");
        assert!(!gate_flipped(&f, false, 0));
    }

    #[test]
    fn passes_short_single_line_reasons_through_unchanged() {
        assert_eq!(
            summarize_reason("gated pending a go/no-go decision"),
            "gated pending a go/no-go decision"
        );
    }

    #[test]
    fn takes_the_last_non_blank_line_of_a_multiline_reason() {
        // Item #269-shaped: a raw stdout/JSON dump followed by the actual
        // error on its own trailing line (`diagnostic_suffix`'s shape).
        let reason = "{\"type\":\"assistant\",\"text\":\"...\"}\n\n\nerror: token expired";
        assert_eq!(summarize_reason(reason), "error: token expired");
    }

    #[test]
    fn caps_an_overlong_line_and_marks_the_cut() {
        let long_line = "x".repeat(NOTIFY_REASON_MAX_CHARS + 50);
        let summarized = summarize_reason(&long_line);
        assert_eq!(
            summarized.chars().count(),
            NOTIFY_REASON_MAX_CHARS + 1 // + the "…" marker
        );
        assert!(summarized.ends_with('…'));
    }
}
