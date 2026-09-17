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

/// Best-effort Telegram ping for an item that just landed on a human gate
/// (a go/no-go decision, an unanswerable question, or a CI self-repair cap).
/// Silently does nothing when `TELEGRAM_NOTIFY_CHAT_ID_SECRET` isn't
/// configured, since notifications are opt-in and a bare install shouldn't
/// spam stderr every tick; a configured-but-failing send only logs -- a
/// notification failure must never block the gate itself.
pub(crate) fn notify_human_gate(item: &agentflare_backend::item::Item, reason: &str) {
    if test_notify_disabled() {
        return;
    }
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    let reason = summarize_reason(reason);
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
    if test_notify_disabled() {
        return;
    }
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
