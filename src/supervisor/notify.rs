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
/// (`notify_human_gate`, `request_channel_approval`). `cargo test --bin
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
    if test_notify_disabled() {
        return;
    }
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    let reason = summarize_reason(reason);
    let text = format!(
        "\u{1F514} <b>agentflare</b> needs a human\n\
         <b>Item:</b> #{} \u{2014} {}\n\
         {}",
        item.sequence_id,
        html_escape(&item.name),
        html_escape(&reason),
    );
    // Same per-chat lock `chat_channel::run_chat_turn` holds for its whole
    // body -- without it, this side-channel send races an in-flight chat
    // reply to the same chat with no ordering between the two Telegram API
    // calls, so an unrelated notification can land interleaved with it
    // (item #281).
    let turn_lock = crate::chat_channel::chat_turn_lock(&chat_id);
    let _turn_guard = turn_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Err(e) = crate::channels::send_telegram_card(&chat_id, &text, &[]) {
        eprintln!(
            "agentflare-supervisor: telegram notify failed for item #{}: {e}",
            item.sequence_id
        );
    }
}

/// Shared card-send half of [`notify_pr_approval_gate`] and
/// [`notify_plan_approval_gate`]: look up the configured notify chat and
/// send a one-button Telegram card. Same fail-open contract as
/// `notify_human_gate`: no-ops without a configured chat id, and a send
/// failure only logs -- a notification failure must never block the gate
/// itself.
fn request_channel_approval(card_text: &str, button: Option<(&str, &str)>) {
    if test_notify_disabled() {
        return;
    }
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    // See the matching lock in `notify_human_gate` above: without it, this
    // card send races `chat_channel::run_chat_turn`'s reply to the same
    // chat, so e.g. a plan-approval card can land interleaved with an
    // unrelated in-flight conversational reply (item #281).
    let turn_lock = crate::chat_channel::chat_turn_lock(&chat_id);
    let _turn_guard = turn_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let buttons: Vec<(&str, &str)> = button.into_iter().collect();
    if let Err(e) = crate::channels::send_telegram_card(&chat_id, card_text, &buttons) {
        eprintln!("agentflare-supervisor: telegram card notify failed: {e}");
    }
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

/// The HTML card body for [`notify_pr_approval_gate`], split out so its
/// wording is testable without a Telegram chat.
fn pr_approval_card_text(
    item: &agentflare_backend::item::Item,
    repo: &crate::github::RepoId,
    number: u64,
    blocker: PrApprovalBlocker,
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
    format!(
        "\u{1F514} <b>agentflare</b> needs a human\n\
         <b>Repo:</b> {repo}\n\
         <b>Item:</b> #{} \u{2014} {}\n\
         {}\n\n\
         {status}",
        item.sequence_id,
        html_escape(&item.name),
        html_escape(&excerpt),
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
) {
    let Some(repo) = crate::github::RepoId::resolve_from_remote(std::path::Path::new(folder_path))
    else {
        return;
    };
    let text = pr_approval_card_text(item, &repo, number, blocker);
    let wants_label = !matches!(
        blocker,
        PrApprovalBlocker::GitHubReview {
            label_missing: false,
            ..
        }
    );
    let callback = format!("approve:{repo}#{number}");
    request_channel_approval(
        &text,
        wants_label.then_some(("\u{2705} Approve", callback.as_str())),
    );
}

/// Human-approval card for a submitted plan -- the plan-gate sibling of
/// `notify_pr_approval_gate`. `plan_asset_id` is included in the card text
/// (not the callback data, which stays a plain item reference) purely so
/// a human reading the notification knows which asset to open. Same
/// fail-open contract as `notify_human_gate`: no-ops without a configured
/// chat id, and a send failure only logs.
pub(crate) fn notify_plan_approval_gate(
    item: &agentflare_backend::item::Item,
    plan_asset_id: &str,
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
    request_channel_approval(&text, Some(("\u{2705} Approve", callback.as_str())));
}

/// Parse an "Approve" button's `callback_data` (`approve_plan:{item_id}`)
/// built by [`notify_plan_approval_gate`]. The inbound half lives in
/// `supervisor::telegram::handle_telegram_callback`, alongside
/// `parse_approve_callback`'s PR-approval handling -- both callback flavors
/// share the one Telegram poller, so they share the one dispatch point.
pub(crate) fn parse_plan_approve_callback(data: &str) -> Option<String> {
    data.strip_prefix("approve_plan:").map(str::to_string)
}

/// True the first time a given item id is seen gated since this process
/// started, false on every later call for the same id -- `run_discovery_tick`
/// re-visits an already-gated item on every tick (it stays in the
/// `ready-for-work` query until a human clears `NEEDS_DECISION_LABEL`), so
/// this keeps `notify_human_gate` firing once per gate instead of once per
/// tick. In-memory and per-process by design: a daemon restart re-notifies
/// once, which is preferable to a persistent marker for a one-line ping.
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
    fn label_blocker_asks_for_the_approval_label() {
        let text = pr_approval_card_text(&item(), &repo(), 7, PrApprovalBlocker::ApprovalLabel);
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
