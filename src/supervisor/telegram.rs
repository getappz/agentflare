//! Inbound Telegram polling: the single `getUpdates` poller shared by the
//! PR-approval-card flow, the plan-approval-card flow (both handled by
//! `handle_telegram_callback`, dispatched on the callback data's prefix),
//! and the chat channel (`handle_chat_message`) -- split out of
//! `supervisor.rs` once this grew large enough on its own to push that file
//! over the LOC gate. Everything here is still very much part of the
//! supervisor: `PR_APPROVAL_LABEL` and `TELEGRAM_NOTIFY_CHAT_ID_SECRET` stay
//! defined in `super`, and the outbound card-sending half (including
//! `parse_plan_approve_callback`'s counterpart, `notify_plan_approval_gate`)
//! lives in the sibling `notify` module.

use super::{PR_APPROVAL_LABEL, TELEGRAM_NOTIFY_CHAT_ID_SECRET};

/// `vault` secret persisting the last-consumed Telegram `update_id` across
/// restarts -- offset semantics per Telegram's own `getUpdates` docs (pass
/// `last_update_id + 1` to avoid redelivery). Reuses the vault store as a
/// convenient single-value KV, same precedent as
/// `TELEGRAM_NOTIFY_CHAT_ID_SECRET` -- neither value is actually secret.
const TELEGRAM_UPDATE_OFFSET_SECRET: &str = "telegram_update_offset";

/// `next` values (Telegram `update_id + 1`) for updates that have been
/// dispatched but haven't *settled* yet -- a free-text turn still running on
/// its background thread, or (very briefly) a synchronous command/callback
/// mid-handler. `persist_offset_if_advanced` never advances the persisted
/// offset past the smallest value in here, so a crash before an in-flight
/// free-text turn finishes leaves the offset short of it: the next poll
/// re-fetches and re-dispatches that update instead of silently losing it
/// (the actual race the old per-update-at-dispatch-time write had). It also
/// lets `poll_telegram_approvals` recognize an update it already dispatched
/// -- Telegram keeps redelivering an unconfirmed update every tick -- and
/// skip spawning a second turn for the same message.
pub(crate) static IN_FLIGHT_UPDATE_OFFSETS: std::sync::Mutex<std::collections::BTreeSet<i64>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Highest `next` offset this process has ever seen from `getUpdates`,
/// regardless of whether it has settled yet -- the ceiling
/// `persist_offset_if_advanced` advances to once nothing is in flight below
/// it.
static SEEN_OFFSET_CEILING: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Cache of the value last written to `TELEGRAM_UPDATE_OFFSET_SECRET`, so
/// `persist_offset_if_advanced` -- called once per poll tick rather than
/// once per update -- can skip the vault write (a cross-process-locked
/// read/decrypt/re-encrypt/rewrite) when nothing has actually advanced.
/// `None` until this process has read or written the offset at least once.
static LAST_PERSISTED_OFFSET: std::sync::Mutex<Option<i64>> = std::sync::Mutex::new(None);

/// Mark `next` as dispatched-but-not-settled.
pub(crate) fn mark_in_flight(next: i64) {
    IN_FLIGHT_UPDATE_OFFSETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(next);
}

/// Mark `next` as settled (its handling -- sync or async -- has fully
/// finished), making it eligible for the persisted offset to advance past.
/// Does not itself write to the vault; `poll_telegram_approvals` batches
/// that into one `persist_offset_if_advanced` call per tick, which also
/// picks up anything settled by a prior tick's still-running background
/// turn.
pub(crate) fn settle(next: i64) {
    IN_FLIGHT_UPDATE_OFFSETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&next);
}

/// The highest offset that's safe to confirm right now: `ceiling` if
/// nothing is in flight, else capped just below the earliest still
/// in-flight update -- confirming any further would drop an update whose
/// handling isn't done yet. Pure and vault-free so it's directly
/// unit-testable; `persist_offset_if_advanced` is the only caller.
/// Delegates to `flare-channels` so the crate and the supervisor share one
/// implementation (the crate's copy carries the unit tests).
pub(crate) fn safe_offset_to_persist(
    ceiling: i64,
    in_flight: &std::collections::BTreeSet<i64>,
) -> i64 {
    flare_channels::safe_offset_to_persist(ceiling, in_flight)
}

/// Persist `safe_offset_to_persist`'s result if it's an improvement on
/// what's already on file. Called once per poll tick (after its whole
/// batch settles or is dispatched), so a failure here logs at most once
/// per tick rather than once per update in the batch.
fn persist_offset_if_advanced() {
    let ceiling = SEEN_OFFSET_CEILING.load(std::sync::atomic::Ordering::Acquire);
    let safe = safe_offset_to_persist(
        ceiling,
        &IN_FLIGHT_UPDATE_OFFSETS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );

    let mut last = LAST_PERSISTED_OFFSET
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if last.is_some_and(|persisted| safe <= persisted) {
        return;
    }
    match crate::vault::set_secret(TELEGRAM_UPDATE_OFFSET_SECRET, &safe.to_string()) {
        Ok(()) => *last = Some(safe),
        Err(e) => {
            eprintln!("agentflare-supervisor: failed to persist telegram update offset: {e}");
        }
    }
}

/// Parse an "Approve" button's `callback_data` (`approve:{owner}/{repo}#{number}`)
/// built by `notify_pr_approval_gate`.
pub(crate) fn parse_approve_callback(data: &str) -> Option<(crate::github::RepoId, u64)> {
    let rest = data.strip_prefix("approve:")?;
    let (repo_part, number_part) = rest.rsplit_once('#')?;
    let repo = crate::github::RepoId::parse(repo_part)?;
    let number: u64 = number_part.parse().ok()?;
    Some((repo, number))
}

/// Poll for Telegram button clicks and act on any "Approve" tap by adding
/// `PR_APPROVAL_LABEL` to the PR it names -- the inbound half of
/// `notify_pr_approval_gate`'s card. Runs from its own fixed-interval
/// supervisor tick (`spawn_supervisor_telegram_approvals`), the same shape
/// as `run_discovery_tick`/`run_review_sweep`, rather than a dedicated
/// long-poll thread. No-ops without a configured chat id (opt-in, same as
/// every other Telegram path here); a single bad update is logged and
/// skipped rather than wedging the whole poll loop.
///
/// Fetches both `callback_query` (the `approve:` card flow, handled by
/// [`handle_telegram_callback`] below unchanged) and `message` updates (the
/// chat channel's commands/free-text prompts, handled by
/// [`handle_chat_message`]) in the SAME call, sharing this one offset.
/// Telegram allows exactly one `getUpdates` poller per bot token, and
/// advancing the offset confirms (and permanently drops) every update below
/// it regardless of which `allowed_updates` filter fetched them — so two
/// independently-offset pollers on one token can silently steal updates
/// from each other. Keeping this as the single poller is required, not
/// just tidier.
pub(crate) fn poll_telegram_approvals(mcp: std::sync::Arc<crate::mcp_server::AgentflareMcp>) {
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    // One-time startup (token validation, webhook clearing, command menu).
    // Non-fatal and self-healing: runs once per process on success, retries
    // on later ticks otherwise. The poll below runs regardless.
    crate::channels::ensure_telegram_ready();
    let offset: i64 = crate::vault::get_secret(TELEGRAM_UPDATE_OFFSET_SECRET)
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    {
        let mut last = LAST_PERSISTED_OFFSET
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *last = Some(last.map_or(offset, |l| l.max(offset)));
    }
    SEEN_OFFSET_CEILING.fetch_max(offset, std::sync::atomic::Ordering::AcqRel);

    let updates = match crate::channels::get_telegram_updates_filtered(
        offset,
        &["callback_query", "message"],
    ) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("agentflare-supervisor: telegram getUpdates failed: {e}");
            return;
        }
    };
    for update in &updates {
        let Some(next) = update
            .get("update_id")
            .and_then(serde_json::Value::as_i64)
            .map(flare_channels::next_offset)
        else {
            continue;
        };
        if next <= offset {
            continue;
        }
        SEEN_OFFSET_CEILING.fetch_max(next, std::sync::atomic::Ordering::AcqRel);
        let already_in_flight = IN_FLIGHT_UPDATE_OFFSETS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&next);
        if already_in_flight {
            // Dispatched in an earlier tick, still running (or Telegram
            // redelivered before the offset caught up) -- don't spawn a
            // second turn for the same message.
            continue;
        }
        mark_in_flight(next);
        if update.get("callback_query").is_some() {
            handle_telegram_callback(update, &chat_id, &mcp);
            settle(next);
        } else if let Some(message) = update.get("message") {
            handle_chat_message(message, &chat_id, &mcp, next);
        } else {
            settle(next);
        }
    }
    // Once per tick, not once per update: also picks up anything a
    // still-running free-text turn from a *prior* tick has since settled.
    persist_offset_if_advanced();
}

/// Handle one `message` update: verify it came from the configured notify
/// chat (same authorization `handle_telegram_callback` applies below), then
/// hand the text off to `chat_channel::dispatch_message`. A message with no
/// `chat`/`text` field, or from any other chat, is silently skipped.
///
/// For free text, `dispatch_message` spawns the actual agent turn on a
/// background thread; `update_offset` (this update's `next` value) stays in
/// `IN_FLIGHT_UPDATE_OFFSETS` -- via the `on_settled` callback below -- until
/// that turn truly finishes, so `persist_offset_if_advanced` can't confirm
/// it early. Every return path here must settle `update_offset` exactly
/// once (the caller already called `mark_in_flight`), including the early
/// returns below for an unauthorized/malformed message, or it would stay
/// in flight forever and permanently block the offset from advancing past
/// it. Slash commands (`chat_new` et al.) settle synchronously:
/// `dispatch_message` runs those before returning, calling `on_settled`
/// itself.
pub(crate) fn handle_chat_message(
    message: &serde_json::Value,
    expected_chat_id: &str,
    mcp: &std::sync::Arc<crate::mcp_server::AgentflareMcp>,
    update_offset: i64,
) {
    let Some(chat_id) = message
        .get("chat")
        .and_then(|c| c.get("id"))
        .map(std::string::ToString::to_string)
    else {
        settle(update_offset);
        return;
    };
    // Single-member allowlist: only the configured notify chat may drive
    // the agent (DmPolicy::AllowedOnly with one member). Spelled through
    // the shared policy helper so a future multi-user allowlist changes
    // data (the member set), not this branch.
    if !flare_channels::dm_allows(
        flare_channels::DmPolicy::AllowedOnly,
        chat_id == expected_chat_id,
    ) {
        settle(update_offset);
        return;
    }
    let Some(text) = message.get("text").and_then(serde_json::Value::as_str) else {
        settle(update_offset); // non-text message (photo, sticker, ...) -- not handled yet
        return;
    };
    let text = text.trim();
    if text.is_empty() {
        settle(update_offset);
        return;
    }
    // Realtime mirror of this authorized inbound message for the dashboard
    // stream and future gateway consumers. `update_offset` is this update's
    // `next` value, so the event id is `update_id`, matching the poller.
    // `sender` prefers the Telegram user id and falls back to the chat id
    // (equal for the authorized-DM flow this daemon serves today).
    let update_id = update_offset - 1;
    let sender = message
        .get("from")
        .and_then(|f| f.get("id"))
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| chat_id.clone());
    crate::channels::chat_bus().publish(flare_channels::ChannelEvent::Inbound(
        flare_channels::ChannelMessage::text(
            update_id.to_string(),
            sender,
            chat_id.clone(),
            text.to_string(),
            flare_channels::TELEGRAM_CHANNEL_NAME,
        ),
    ));
    crate::chat_channel::dispatch_message(
        crate::chat_channel::telegram_channel(),
        chat_id,
        text.to_string(),
        mcp,
        move || {
            settle(update_offset);
            crate::channels::chat_bus().publish(flare_channels::ChannelEvent::Settled {
                channel: flare_channels::TELEGRAM_CHANNEL_NAME.to_string(),
                id: update_id.to_string(),
            });
        },
    );
}

/// Handle one `callback_query` update: verify it came from the configured
/// notify chat (the only chat a card was ever sent to, but checked anyway --
/// defense in depth against, e.g., the bot later being added to a group),
/// then dispatch on its callback data prefix -- `approve_plan:` (a plan
/// approval card, see `notify::notify_plan_approval_gate`) or `approve:` (a
/// PR-approval card) -- add the label/approve the plan, then ack + strip the
/// button so a repeat tap is a no-op rather than a duplicate GitHub/DB call.
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
    if let Some(item_id) = super::parse_plan_approve_callback(data) {
        // `item_approve_plan_via_channel`, NOT the public `item_approve_plan`:
        // this tap came from a human in the configured notify chat (verified
        // above), which is the one legitimate way a `plan_approver == "human"`
        // item gets approved. The public method refuses those outright so no
        // agent can self-approve (item #573 final review).
        let succeeded =
            match mcp.item_approve_plan_via_channel(crate::mcp_server::types::ItemRequest {
                action: "approve_plan".into(),
                id: Some(item_id.clone()),
                ..Default::default()
            }) {
                Ok(_) => true,
                Err(e) => {
                    eprintln!(
                        "agentflare-supervisor: telegram plan-approve for {item_id} failed: {e}"
                    );
                    false
                }
            };
        let ack_text = if succeeded {
            "\u{2705} Approved".to_string()
        } else {
            "failed -- tap Approve again to retry".to_string()
        };
        let _ = crate::channels::answer_telegram_callback(callback_id, &ack_text);
        // Only strip the button once approval actually landed -- a transient
        // failure (e.g. a DB error) must leave the human a way to retry
        // instead of stranding the item with no way to re-tap Approve
        // (CodeRabbit finding on item #573's PR).
        if succeeded
            && let Some(message_id) = message
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
