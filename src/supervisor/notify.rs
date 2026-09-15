//! Human-notification and channel-approval half of the supervisor: the
//! Telegram pings it sends when an item lands on a gate, and the inbound
//! poll that turns a human's "Approve" tap back into an action.
//!
//! Split out of `supervisor.rs` when item #573's plan-gate work pushed that
//! file past the 1500-line LOC gate (`scripts/loc-gate.sh`). This is a pure
//! move: every item here is re-exported by the parent as
//! `pub(crate) use notify::*`, so existing `crate::supervisor::…` paths --
//! and `supervisor_tests.rs`'s `use super::*` -- are unchanged.
//!
//! Everything here is fail-open by contract: each function no-ops when
//! `TELEGRAM_NOTIFY_CHAT_ID_SECRET` isn't configured (notifications are
//! opt-in), and a configured-but-failing send only logs. A notification
//! failure must never block the gate it is reporting.

use super::*;

/// Best-effort Telegram ping for an item that just landed on a human gate
/// (a go/no-go decision, an unanswerable question, or a CI self-repair cap).
/// Silently does nothing when `TELEGRAM_NOTIFY_CHAT_ID_SECRET` isn't
/// configured, since notifications are opt-in and a bare install shouldn't
/// spam stderr every tick; a configured-but-failing send only logs -- a
/// notification failure must never block the gate itself.
pub(crate) fn notify_human_gate(item: &agentflare_backend::item::Item, reason: &str) {
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    let text = format!(
        "agentflare: item #{} ({}) needs a human -- {reason}",
        item.sequence_id, item.id
    );
    if let Err(e) =
        crate::channels::send_message(crate::channels::Platform::Telegram, &chat_id, &text)
    {
        eprintln!(
            "agentflare-supervisor: telegram notify failed for item #{}: {e}",
            item.sequence_id
        );
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

/// Shared card-send half of [`notify_pr_approval_gate`] and
/// [`notify_plan_approval_gate`]: look up the configured notify chat and
/// send a one-button Telegram card. Same fail-open contract as
/// `notify_human_gate`: no-ops without a configured chat id, and a send
/// failure only logs -- a notification failure must never block the gate
/// itself.
fn request_channel_approval(card_text: &str, approve_label: &str, callback_data: &str) {
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    if let Err(e) =
        crate::channels::send_telegram_card(&chat_id, card_text, &[(approve_label, callback_data)])
    {
        eprintln!("agentflare-supervisor: telegram card notify failed: {e}");
    }
}

/// Telegram-only rich variant of [`notify_human_gate`] for the one gate a
/// human can resolve with a single tap: CI is green and the only thing
/// missing is `PR_APPROVAL_LABEL`. Unlike the plain-text pings, this carries
/// an inline "Approve" button whose `callback_data` embeds the repo and PR
/// number directly (`approve:{owner}/{repo}#{number}`) -- self-contained,
/// so [`poll_telegram_approvals`] never needs to re-resolve a worktree path
/// to act on a click. Same fail-open contract as `notify_human_gate`: no-ops
/// without a configured chat id or a resolvable repo, and a send failure
/// only logs.
pub(crate) fn notify_pr_approval_gate(
    item: &agentflare_backend::item::Item,
    folder_path: &str,
    number: u64,
) {
    let Some(repo) = crate::github::RepoId::resolve_from_remote(std::path::Path::new(folder_path))
    else {
        return;
    };
    let excerpt: String = item.description.chars().take(200).collect();
    let text = format!(
        "\u{1F514} <b>agentflare</b> needs a human\n\
         <b>Repo:</b> {repo}\n\
         <b>Item:</b> #{} \u{2014} {}\n\
         {}\n\n\
         PR <a href=\"https://github.com/{repo}/pull/{number}\">#{number}</a> is CI-green and \
         mergeable, awaiting <code>{PR_APPROVAL_LABEL}</code>.",
        item.sequence_id,
        html_escape(&item.name),
        html_escape(&excerpt),
    );
    request_channel_approval(
        &text,
        "\u{2705} Approve",
        &format!("approve:{repo}#{number}"),
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
    request_channel_approval(
        &text,
        "\u{2705} Approve",
        &format!("approve_plan:{}", item.id),
    );
}

/// Parse an "Approve" button's `callback_data` (`approve:{owner}/{repo}#{number}`)
/// built by [`notify_pr_approval_gate`].
fn parse_approve_callback(data: &str) -> Option<(crate::github::RepoId, u64)> {
    let rest = data.strip_prefix("approve:")?;
    let (repo_part, number_part) = rest.rsplit_once('#')?;
    let repo = crate::github::RepoId::parse(repo_part)?;
    let number: u64 = number_part.parse().ok()?;
    Some((repo, number))
}

/// Parse an "Approve" button's `callback_data` (`approve_plan:{item_id}`)
/// built by [`notify_plan_approval_gate`].
fn parse_plan_approve_callback(data: &str) -> Option<String> {
    data.strip_prefix("approve_plan:").map(str::to_string)
}

/// Poll for Telegram button clicks and act on any "Approve" tap by adding
/// `PR_APPROVAL_LABEL` to the PR it names -- the inbound half of
/// `notify_pr_approval_gate`'s card. Runs from its own fixed-interval
/// supervisor tick (`spawn_supervisor_telegram_approvals`), the same shape
/// as `run_discovery_tick`/`run_review_sweep`, rather than a dedicated
/// long-poll thread. No-ops without a configured chat id (opt-in, same as
/// every other Telegram path here); a single bad update is logged and
/// skipped rather than wedging the whole poll loop.
pub(crate) fn poll_telegram_approvals(mcp: &crate::mcp_server::AgentflareMcp) {
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    let offset: i64 = crate::vault::get_secret(TELEGRAM_UPDATE_OFFSET_SECRET)
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let updates = match crate::channels::get_telegram_updates(offset) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("agentflare-supervisor: telegram getUpdates failed: {e}");
            return;
        }
    };
    let mut next_offset = offset;
    for update in &updates {
        if let Some(id) = update.get("update_id").and_then(serde_json::Value::as_i64) {
            next_offset = next_offset.max(id + 1);
        }
        handle_telegram_callback(update, &chat_id, mcp);
    }
    if next_offset != offset
        && let Err(e) =
            crate::vault::set_secret(TELEGRAM_UPDATE_OFFSET_SECRET, &next_offset.to_string())
    {
        eprintln!("agentflare-supervisor: failed to persist telegram update offset: {e}");
    }
}

/// Handle one `callback_query` update: verify it came from the configured
/// notify chat (the only chat a card was ever sent to, but checked anyway --
/// defense in depth against, e.g., the bot later being added to a group),
/// parse its `approve:` callback data, add the label, then ack + strip the
/// button so a repeat tap is a no-op rather than a duplicate GitHub call.
pub(crate) fn handle_telegram_callback(
    update: &serde_json::Value,
    expected_chat_id: &str,
    mcp: &crate::mcp_server::AgentflareMcp,
) {
    let Some(callback) = update.get("callback_query") else {
        return;
    };
    let Some(data) = callback.get("data").and_then(serde_json::Value::as_str) else {
        return;
    };
    let Some(callback_id) = callback.get("id").and_then(serde_json::Value::as_str) else {
        return;
    };
    let message = callback.get("message");
    let chat_id = message
        .and_then(|m| m.get("chat"))
        .and_then(|c| c.get("id"))
        .map(std::string::ToString::to_string);
    if chat_id.as_deref() != Some(expected_chat_id) {
        return;
    }
    if let Some(item_id) = parse_plan_approve_callback(data) {
        // `item_approve_plan_via_channel`, NOT the public `item_approve_plan`:
        // this tap came from a human in the configured notify chat (verified
        // above), which is the one legitimate way a `plan_approver == "human"`
        // item gets approved. The public method refuses those outright so no
        // agent can self-approve (item #573 final review).
        let ack_text =
            match mcp.item_approve_plan_via_channel(crate::mcp_server::types::ItemRequest {
                action: "approve_plan".into(),
                id: Some(item_id.clone()),
                ..Default::default()
            }) {
                Ok(_) => "\u{2705} Approved".to_string(),
                Err(e) => {
                    eprintln!(
                        "agentflare-supervisor: telegram plan-approve for {item_id} failed: {e}"
                    );
                    format!("failed: {e}")
                }
            };
        let _ = crate::channels::answer_telegram_callback(callback_id, &ack_text);
        if let Some(message_id) = message
            .and_then(|m| m.get("message_id"))
            .and_then(serde_json::Value::as_i64)
        {
            let _ = crate::channels::clear_telegram_reply_markup(expected_chat_id, message_id);
        }
        return;
    }
    let Some((repo, number)) = parse_approve_callback(data) else {
        return;
    };
    let ack_text = match crate::github::Client::new()
        .map_err(|e| e.to_string())
        .and_then(|client| {
            crate::github::issues::add_labels(
                &client,
                &repo,
                number,
                &[PR_APPROVAL_LABEL.to_string()],
            )
            .map_err(|e| e.to_string())
        }) {
        Ok(()) => "\u{2705} Approved".to_string(),
        Err(e) => {
            eprintln!("agentflare-supervisor: telegram approve for {repo}#{number} failed: {e}");
            format!("failed: {e}")
        }
    };
    let _ = crate::channels::answer_telegram_callback(callback_id, &ack_text);
    if let Some(message_id) = message
        .and_then(|m| m.get("message_id"))
        .and_then(serde_json::Value::as_i64)
    {
        let _ = crate::channels::clear_telegram_reply_markup(expected_chat_id, message_id);
    }
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
