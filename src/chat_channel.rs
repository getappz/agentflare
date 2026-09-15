//! Inbound chat channel: lets agentflare be prompted/managed by chatting
//! with it, on top of the existing outbound-only `channels` module. Free
//! text resumes a per-chat headless-agent session (same `--resume`
//! mechanism `work_item_pipeline`'s SDD loop uses), while a leading
//! `/command` is routed to `AgentflareMcp::handle_chat_command` -- the same
//! `item`/`project`/`pm` MCP-tool-shaped operations any other client would
//! call.
//!
//! Polling itself lives in `supervisor::poll_telegram_approvals`, not here:
//! Telegram allows exactly one `getUpdates` poller per bot token (advancing
//! the offset confirms and permanently drops every update below it,
//! regardless of which `allowed_updates` filter fetched them), so the
//! approval flow's `callback_query` poll and this module's `message`
//! handling share that one poll and its one offset. This module owns
//! everything downstream of "here is one chat_id + text": command parsing,
//! per-chat session/turn state, and sending the reply.

use crate::mcp_server::AgentflareMcp;

/// A chat platform capable of replying to a message -- the inbound side's
/// outbound leg (fetching updates is centralized in `supervisor`, see
/// module doc comment). Written as a trait so a second platform
/// (Slack/Discord) only needs a new impl, not changes to the routing logic
/// in this module.
pub trait ChatChannel {
    /// Send a plain-text reply into a chat.
    fn send_reply(&self, chat_id: &str, text: &str) -> Result<(), String>;
}

/// `vault` secret holding the per-chat headless-agent session map (JSON
/// `{chat_id: session_id}`), so free text picks up where the last turn in
/// that chat left off instead of starting a fresh agent session every time.
const TELEGRAM_CHAT_SESSIONS_SECRET: &str = "telegram_chat_sessions";
/// `vault` secret naming which registered agent handles free-text chat
/// turns; defaults to `claude-code` when unset.
const TELEGRAM_CHAT_AGENT_SECRET: &str = "telegram_chat_agent";
const DEFAULT_CHAT_AGENT: &str = "claude-code";

/// Hard cap and idle timeout for a single chat-driven headless agent turn.
/// Mirrors `agentflare run --print`'s own timeout role (`cli_run_headless`):
/// this is a one-off non-autonomous invocation, so neither
/// `agent_registry::autonomous_args` nor any permission-bypass flag is
/// applied -- a chat turn that needs a tool approval it can't get will just
/// run out the clock, same as `--print` already accepts.
const CHAT_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

pub struct TelegramChannel;

impl ChatChannel for TelegramChannel {
    fn send_reply(&self, chat_id: &str, text: &str) -> Result<(), String> {
        crate::channels::send_message(crate::channels::Platform::Telegram, chat_id, text)
    }
}

/// Load the per-chat session map, keyed by chat id.
fn load_sessions() -> std::collections::HashMap<String, String> {
    crate::vault::get_secret(TELEGRAM_CHAT_SESSIONS_SECRET)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Guards the read-modify-write in `save_session` -- `crate::vault::set_secret`
/// itself does a plain read-then-write of the whole vault body with no
/// locking, so two chats' turns saving a session at the same moment could
/// otherwise silently lose one of the two updates to the shared session map.
/// Held only for the brief load+insert+write, never across an agent run.
static SESSIONS_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn save_session(chat_id: &str, session_id: &str) {
    let _guard = SESSIONS_WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut sessions = load_sessions();
    sessions.insert(chat_id.to_string(), session_id.to_string());
    if let Ok(encoded) = serde_json::to_string(&sessions) {
        let _ = crate::vault::set_secret(TELEGRAM_CHAT_SESSIONS_SECRET, &encoded);
    }
}

/// Drop a chat's session mapping (e.g. once it's confirmed stale) so the
/// next turn starts fresh instead of repeating a doomed `--resume`.
fn remove_session(chat_id: &str) {
    let _guard = SESSIONS_WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut sessions = load_sessions();
    if sessions.remove(chat_id).is_some()
        && let Ok(encoded) = serde_json::to_string(&sessions)
    {
        let _ = crate::vault::set_secret(TELEGRAM_CHAT_SESSIONS_SECRET, &encoded);
    }
}

/// Per-chat turn lock, keyed by chat id -- `run_chat_turn` holds this for its
/// whole body so a second free-text message for the same chat (routine: the
/// 20s `SUPERVISOR_TELEGRAM_POLL_INTERVAL` poll is well under
/// `CHAT_TURN_TIMEOUT`) queues behind the first instead of racing it: without
/// this, the second turn's `load_sessions()` could run before the first
/// turn's `save_session()`, resuming from a stale session id and then
/// clobbering it once both finish. Different chats never block each other --
/// only same-chat turns share a lock.
fn chat_turn_lock(chat_id: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(chat_id.to_string())
        .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
        .clone()
}

fn chat_agent() -> String {
    crate::vault::get_secret(TELEGRAM_CHAT_AGENT_SECRET)
        .ok()
        .flatten()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CHAT_AGENT.to_string())
}

/// Resume args (`--resume <id>`, if the agent supports it and a prior
/// session is on file for this chat) for one chat turn.
fn resume_args(
    agent_name: &str,
    chat_id: &str,
    sessions: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let Some(agent) = agent_registry::agent_by_name(agent_name) else {
        return Vec::new();
    };
    let Some(flag) = agent_registry::resume_arg(agent) else {
        return Vec::new();
    };
    match sessions.get(chat_id) {
        Some(session_id) => vec![flag.to_string(), session_id.clone()],
        None => Vec::new(),
    }
}

/// Run one free-text chat turn: resume the chat's headless-agent session
/// (if any), persist whatever new session id comes back, and reply. Retries
/// once with a fresh session if the resumed one has gone stale (e.g. the
/// daemon restarted since -- item #159's failure mode). Serializes with any
/// other turn for the same chat id via `chat_turn_lock` (see its doc
/// comment) so concurrent messages in one chat can't race each other's
/// session state.
fn run_chat_turn(channel: &dyn ChatChannel, chat_id: String, prompt: String) {
    let turn_lock = chat_turn_lock(&chat_id);
    let _turn_guard = turn_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let agent_name = chat_agent();
    let sessions = load_sessions();
    let extra_args = resume_args(&agent_name, &chat_id, &sessions);

    let mut outcome = crate::agent_launch::run_headless(
        agent_registry::REGISTRY,
        &agent_name,
        &prompt,
        CHAT_TURN_TIMEOUT,
        CHAT_TURN_TIMEOUT,
        &extra_args,
        true,
    );
    if !extra_args.is_empty()
        && let crate::agent_launch::HeadlessOutcome::Failed(ref msg) = outcome
        && crate::work_item_pipeline::is_stale_session_error(msg)
    {
        // Drop the dead session before retrying fresh, not just on a
        // subsequent success (below) -- otherwise a fresh retry that also
        // fails leaves the stale id on file, so every later message in this
        // chat repeats this same doomed resume attempt first.
        remove_session(&chat_id);
        outcome = crate::agent_launch::run_headless(
            agent_registry::REGISTRY,
            &agent_name,
            &prompt,
            CHAT_TURN_TIMEOUT,
            CHAT_TURN_TIMEOUT,
            &[],
            true,
        );
    }

    let reply = match outcome {
        crate::agent_launch::HeadlessOutcome::Ok(reply) => {
            if let Some(session_id) = &reply.session_id {
                save_session(&chat_id, session_id);
            }
            if reply.text.trim().is_empty() {
                "(no reply)".to_string()
            } else {
                reply.text
            }
        }
        crate::agent_launch::HeadlessOutcome::UnknownAgent(msg)
        | crate::agent_launch::HeadlessOutcome::NotHeadless(msg)
        | crate::agent_launch::HeadlessOutcome::NotFound(msg)
        | crate::agent_launch::HeadlessOutcome::Failed(msg) => format!("agent turn failed: {msg}"),
    };
    if let Err(e) = channel.send_reply(&chat_id, &reply) {
        eprintln!("agentflare-supervisor: chat reply to {chat_id} failed: {e}");
    }
}

/// Split a slash command out of a message's leading token, e.g.
/// `"/new fix the thing"` -> `Some(("new", "fix the thing"))`. `None` for
/// anything that isn't a `/command` (free text).
fn parse_command(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    let rest = text.strip_prefix('/')?;
    let (command, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    // A Telegram slash command can carry a `@botname` suffix (group chats
    // where multiple bots share `/status`) -- strip it before matching.
    let command = command.split('@').next().unwrap_or(command);
    Some((command, args.trim()))
}

/// Hard cap on concurrent free-text chat turns across all chats. Each one
/// is a raw `std::thread::spawn` (see `dispatch_message`) with nothing else
/// bounding how many can pile up if messages arrive faster than
/// `CHAT_TURN_TIMEOUT` turns finish -- `std::thread::spawn` panics once the
/// OS can't create another thread, so an unbounded burst (Telegram can
/// return up to 100 updates per poll) is a real resource-exhaustion path,
/// not just a theoretical one. Comfortably above anything the single
/// authorized chat (`TELEGRAM_NOTIFY_CHAT_ID_SECRET`) produces in normal
/// use, while still capping the worst case.
const MAX_CONCURRENT_CHAT_TURNS: usize = 8;
static IN_FLIGHT_CHAT_TURNS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Releases its `IN_FLIGHT_CHAT_TURNS` reservation on every exit path
/// (normal return or panic) once a turn finishes.
struct ChatTurnSlot;

impl Drop for ChatTurnSlot {
    fn drop(&mut self) {
        IN_FLIGHT_CHAT_TURNS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Reserve a chat-turn slot if under `MAX_CONCURRENT_CHAT_TURNS`, else
/// `None` -- the caller replies with a busy message instead of spawning.
fn try_reserve_chat_turn_slot() -> Option<ChatTurnSlot> {
    IN_FLIGHT_CHAT_TURNS
        .fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |n| (n < MAX_CONCURRENT_CHAT_TURNS).then_some(n + 1),
        )
        .ok()
        .map(|_| ChatTurnSlot)
}

/// Dispatch one already-authorized inbound message: a `/command` is handled
/// inline (fast, in-process MCP-shaped calls) and replied to immediately;
/// free text is handed off to a background thread running the headless
/// agent turn, so a slow agent reply never delays the poll tick that called
/// this (see `supervisor::poll_telegram_approvals`, which also owns
/// authorizing `chat_id` against the configured notify chat before this is
/// ever called). Refuses to spawn past `MAX_CONCURRENT_CHAT_TURNS`.
pub(crate) fn dispatch_message(
    channel: &'static (dyn ChatChannel + Sync),
    chat_id: String,
    text: String,
    mcp: &AgentflareMcp,
) {
    match parse_command(&text) {
        Some((command, args)) => {
            let reply = mcp.handle_chat_command(command, args);
            if let Err(e) = channel.send_reply(&chat_id, &reply) {
                eprintln!("agentflare-supervisor: chat command reply to {chat_id} failed: {e}");
            }
        }
        None => match try_reserve_chat_turn_slot() {
            Some(slot) => {
                std::thread::spawn(move || {
                    let _slot = slot;
                    run_chat_turn(channel, chat_id, text);
                });
            }
            None => {
                if let Err(e) = channel.send_reply(
                    &chat_id,
                    "still processing other requests -- try again in a moment",
                ) {
                    eprintln!("agentflare-supervisor: chat busy reply to {chat_id} failed: {e}");
                }
            }
        },
    }
}

/// The chat channel this daemon polls -- `'static` so `dispatch_message`
/// can hand it to a spawned background thread for free-text turns without
/// any lifetime/ownership machinery; `TelegramChannel` is a stateless unit
/// struct, so a single shared reference is all any caller ever needs.
pub(crate) fn telegram_channel() -> &'static (dyn ChatChannel + Sync) {
    static CHANNEL: TelegramChannel = TelegramChannel;
    &CHANNEL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_splits_command_and_args() {
        assert_eq!(
            parse_command("/new fix the bug"),
            Some(("new", "fix the bug"))
        );
        assert_eq!(parse_command("/status"), Some(("status", "")));
        assert_eq!(parse_command("  /help  "), Some(("help", "")));
    }

    #[test]
    fn parse_command_strips_botname_suffix() {
        assert_eq!(
            parse_command("/status@agentflare_bot"),
            Some(("status", ""))
        );
    }

    #[test]
    fn parse_command_is_none_for_free_text() {
        assert_eq!(parse_command("what's the status of item 42?"), None);
    }

    #[test]
    fn chat_turn_slot_bounds_concurrency_and_releases_on_drop() {
        // Only this test touches IN_FLIGHT_CHAT_TURNS, so a clean starting
        // count is safe to assume even under cargo test's default
        // cross-test parallelism.
        let mut slots = Vec::new();
        for _ in 0..MAX_CONCURRENT_CHAT_TURNS {
            slots.push(try_reserve_chat_turn_slot().expect("should be under the cap"));
        }
        assert!(
            try_reserve_chat_turn_slot().is_none(),
            "must refuse once MAX_CONCURRENT_CHAT_TURNS are already held"
        );
        drop(slots);
        assert!(
            try_reserve_chat_turn_slot().is_some(),
            "dropping a slot must release its reservation"
        );
    }

    #[test]
    fn chat_turn_lock_is_shared_per_chat_and_independent_across_chats() {
        let a1 = chat_turn_lock("chat-a");
        let a2 = chat_turn_lock("chat-a");
        let b = chat_turn_lock("chat-b");
        assert!(
            std::sync::Arc::ptr_eq(&a1, &a2),
            "same chat id must share one lock so turns for it serialize"
        );
        assert!(
            !std::sync::Arc::ptr_eq(&a1, &b),
            "different chat ids must not share a lock"
        );
    }
}
