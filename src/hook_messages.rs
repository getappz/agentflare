//! Agent-message delivery and session registration through host hooks.
//!
//! Every hook invocation registers/refreshes the session it fires for (see
//! [`crate::messages::identity`]). On hosts whose hooks feed text back to the
//! model, pending messages are delivered on the spot: `PreToolUse` (every
//! tool call -- near-realtime while the agent works), `UserPromptSubmit`,
//! `SessionStart`, and `Stop`, which blocks the stop so an agent about to go
//! idle reads the messages that arrived during its turn.

use crate::messages::{self, Message, identity};
use serde_json::{Value, json};

/// Hosts whose hook output reaches the model (`additionalContext`, a Stop
/// hook's block `reason`). Elsewhere a hook only registers the session and
/// never takes a message -- taking marks it delivered, so taking one the
/// host would drop would lose it; those hosts get messages through the MCP
/// result piggyback instead.
pub(crate) fn host_injects_context(agent: &str) -> bool {
    agent == "claude-code"
}

/// The fields every hook's stdin JSON shares.
pub(crate) struct HookSession {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

pub(crate) fn parse_session(input: &str) -> HookSession {
    let v: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    HookSession {
        session_id: s("session_id").or_else(|| s("conversation_id")),
        cwd: s("cwd"),
    }
}

fn now() -> i64 {
    crate::claims::now()
}

/// Registers the hook's session and, on a context-injecting host, takes its
/// pending messages. `register` forces a full registration (SessionStart)
/// and creates the db if needed; otherwise this stays a cheap no-op when
/// there is no db yet. Best-effort: any failure means "no messages".
pub(crate) fn sync(agent: &str, session: &HookSession, register: bool) -> Vec<Message> {
    let key = match (&session.session_id, identity::job_owner()) {
        (_, Some(owner)) => owner,
        (Some(sid), None) => identity::hook_key(agent, sid),
        (None, None) => return vec![],
    };
    let conn = match messages::open_fast() {
        Some(c) => c,
        None if register => match crate::db::open() {
            Ok(c) => c,
            Err(_) => return vec![],
        },
        None => return vec![],
    };
    sync_with(&conn, agent, &key, session.cwd.as_deref(), register, now())
        .or_else(|_| {
            // A table not created yet on a db an older binary made.
            let conn = crate::db::open()?;
            sync_with(&conn, agent, &key, session.cwd.as_deref(), register, now())
        })
        .unwrap_or_default()
}

pub(crate) fn sync_with(
    conn: &rusqlite::Connection,
    agent: &str,
    key: &str,
    cwd: Option<&str>,
    register: bool,
    now: i64,
) -> rusqlite::Result<Vec<Message>> {
    identity::touch_hook_session(conn, key, cwd, register, now)?;
    if !host_injects_context(agent) || !messages::has_undelivered(conn, key)? {
        return Ok(vec![]);
    }
    messages::take_undelivered(conn, key, messages::MAX_BATCH, now)
}

/// Ends the hook's session (SessionEnd). A dispatched job's session is
/// never ended here: one agent turn ending isn't the job ending.
pub(crate) fn end(agent: &str, session: &HookSession) {
    if identity::job_owner().is_some() {
        return;
    }
    let Some(sid) = &session.session_id else {
        return;
    };
    if let Some(conn) = messages::open_fast() {
        let _ = crate::sessions::end(&conn, &identity::hook_key(agent, sid), now());
    }
}

/// PreToolUse output: nudges stay a user-visible `systemMessage`; messages go
/// to the model as `additionalContext`. `None` when there's nothing to say.
pub(crate) fn pre_tool_use_output(msgs: &[Message], nudges: &[String]) -> Option<Value> {
    if msgs.is_empty() && nudges.is_empty() {
        return None;
    }
    let mut out = json!({});
    if !nudges.is_empty() {
        out["systemMessage"] = json!(format!("agentflare: {}", nudges.join(" ")));
    }
    if !msgs.is_empty() {
        out["hookSpecificOutput"] = json!({
            "hookEventName": "PreToolUse",
            "additionalContext": messages::format_delivery(msgs),
        });
    }
    Some(out)
}

/// Stop output: block the stop with the messages as the reason, so the
/// agent keeps going and handles them. Nothing pending -> let it stop.
pub(crate) fn stop_output(msgs: &[Message]) -> Option<Value> {
    (!msgs.is_empty()).then(|| {
        json!({
            "decision": "block",
            "reason": messages::format_delivery(msgs),
        })
    })
}

/// `agentflare hook stop`.
pub fn stop(agent: &str) {
    let Some(input) = crate::hook::read_stdin_or_skip("Stop") else {
        return;
    };
    let msgs = sync(agent, &parse_session(&input), false);
    if let Some(out) = stop_output(&msgs) {
        println!("{out}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions;

    fn conn() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        sessions::migrate(&c).unwrap();
        messages::migrate(&c).unwrap();
        c
    }

    fn no_item(_: &str) -> Result<messages::ItemRoute, String> {
        Err(String::new())
    }

    #[test]
    fn parse_session_reads_session_id_and_cwd() {
        let s = parse_session(r#"{"session_id":"abc","cwd":"/r","hook_event_name":"Stop"}"#);
        assert_eq!(
            (s.session_id.as_deref(), s.cwd.as_deref()),
            (Some("abc"), Some("/r"))
        );
        let cursor = parse_session(r#"{"conversation_id":"c1"}"#);
        assert_eq!(cursor.session_id.as_deref(), Some("c1"));
        assert!(parse_session("not json").session_id.is_none());
    }

    #[test]
    fn sync_registers_the_session_and_delivers_once_on_claude_code() {
        let c = conn();
        let key = "claude-code:s1";
        assert!(
            sync_with(&c, "claude-code", key, Some("/repo"), true, 10)
                .unwrap()
                .is_empty()
        );
        let s = sessions::get(&c, key).unwrap().unwrap();
        assert_eq!(s.cwd.as_deref(), Some("/repo"));

        messages::send(&c, "codex:x", key, "please rebase", None, 11, no_item).unwrap();
        let got = sync_with(&c, "claude-code", key, None, false, 12).unwrap();
        assert_eq!(got.len(), 1);
        assert!(
            sync_with(&c, "claude-code", key, None, false, 13)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn sync_never_takes_on_a_host_that_drops_hook_context() {
        let c = conn();
        messages::send(&c, "a:1", "cursor:s", "hi", None, 1, no_item).unwrap();
        assert!(
            sync_with(&c, "cursor", "cursor:s", None, true, 2)
                .unwrap()
                .is_empty()
        );
        assert!(messages::has_undelivered(&c, "cursor:s").unwrap());
    }

    fn msg(id: i64) -> Message {
        Message {
            id,
            from_key: "codex:x".into(),
            to_key: "claude-code:s1".into(),
            to_address: "claude-code:s1".into(),
            body: "please rebase".into(),
            reply_to: None,
            created_at: 1,
            delivered_at: Some(2),
            read_at: None,
        }
    }

    #[test]
    fn pre_tool_use_output_with_and_without_messages() {
        assert!(pre_tool_use_output(&[], &[]).is_none());
        let only_nudge = pre_tool_use_output(&[], &["batch calls".into()]).unwrap();
        assert_eq!(only_nudge["systemMessage"], "agentflare: batch calls");
        assert!(only_nudge.get("hookSpecificOutput").is_none());

        let out = pre_tool_use_output(&[msg(7)], &["batch calls".into()]).unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        let ctx = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("<agentflare-message from=\"codex:x\" id=7>"));
        assert!(ctx.contains("please rebase"));
        assert!(ctx.contains("NOT from your user"));
        // Never a permission decision: delivery must not change what runs.
        assert!(
            out["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none()
        );
    }

    #[test]
    fn stop_output_blocks_only_with_messages() {
        assert!(stop_output(&[]).is_none());
        let out = stop_output(&[msg(3)]).unwrap();
        assert_eq!(out["decision"], "block");
        assert!(out["reason"].as_str().unwrap().contains("id=3"));
    }
}
