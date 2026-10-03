//! Agent-message delivery and session registration through host hooks.
//!
//! Every hook invocation registers/refreshes the session it fires for (see
//! [`crate::messages::identity`]). On hosts whose hooks feed text back to the
//! model, pending messages are delivered on inject-capable surfaces:
//! Claude/Codex use `PreToolUse` / `UserPromptSubmit` / `SessionStart` /
//! `Stop` (block reason); Cursor uses `sessionStart` / `postToolUse` /
//! `stop` (`followup_message`) -- its `beforeSubmitPrompt` and allow-path
//! `preToolUse` have no model-visible inject field, so mail is held there.

use crate::messages::{self, Message, identity};
use serde_json::{Value, json};

/// Hosts that can put hook output in front of the model on at least one
/// lifecycle surface (`additionalContext` / Cursor `additional_context` /
/// Stop block `reason` / Cursor `followup_message`). Elsewhere a hook only
/// registers the session and never takes a message -- taking marks it
/// delivered, so taking one the host would drop would lose it; those hosts
/// get messages through the MCP result piggyback instead.
///
/// Cursor is included: docs (cursor.com/docs/hooks) confirm model-visible
/// inject on `sessionStart` / `postToolUse` / `stop`. Surfaces without an
/// inject field (`beforeSubmitPrompt`, allow-path `preToolUse`) must pass
/// [`takes_mail_on`] = false so mail is not taken and dropped.
pub(crate) fn host_injects_context(agent: &str) -> bool {
    matches!(agent, "claude-code" | "codex" | "cursor")
}

/// Hook surface that may or may not be allowed to take undelivered mail for
/// a given host (see [`takes_mail_on`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailSurface {
    SessionStart,
    BeforeSubmitPrompt,
    PreToolUse,
    PostToolUse,
    Stop,
}

/// Whether this host+surface may take undelivered mail. Cursor's
/// `beforeSubmitPrompt` and allow-path `preToolUse` have no model-visible
/// inject field, so taking there would lose the message.
pub(crate) fn takes_mail_on(agent: &str, surface: MailSurface) -> bool {
    if !host_injects_context(agent) {
        return false;
    }
    match (agent, surface) {
        ("cursor", MailSurface::BeforeSubmitPrompt | MailSurface::PreToolUse) => false,
        _ => true,
    }
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
    let cwd = s("cwd").or_else(|| {
        v.get("workspace_roots")
            .and_then(Value::as_array)
            .and_then(|roots| roots.first())
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    });
    HookSession {
        session_id: s("session_id").or_else(|| s("conversation_id")),
        cwd,
    }
}

fn now() -> i64 {
    crate::claims::now()
}

/// Which markers a hook may take (spec §5.3). Nothing is ever dropped: a
/// marker a policy skips waits for a later hook or an explicit inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// SessionStart / UserPromptSubmit: everything.
    TurnStart,
    /// PreToolUse: `important`, plus `status` once STATUS_BATCH are pending.
    MidTurn,
    /// Stop: `important` and `status`; never blocks a stop for `fyi`.
    TurnEnd,
}

/// Pending `status` messages it takes before a mid-turn hook delivers them.
pub(crate) const STATUS_BATCH: i64 = 3;

fn markers_for(
    conn: &rusqlite::Connection,
    key: &str,
    policy: Delivery,
) -> rusqlite::Result<Vec<&'static str>> {
    Ok(match policy {
        Delivery::TurnStart => messages::MARKERS.to_vec(),
        Delivery::TurnEnd => vec!["important", "status"],
        Delivery::MidTurn => {
            if messages::count_undelivered_where(conn, key, &["status"])? >= STATUS_BATCH {
                vec!["important", "status"]
            } else {
                vec!["important"]
            }
        }
    })
}

/// Registers the hook's session and, on a context-injecting host, takes its
/// pending messages the `policy` allows. `register` forces a full
/// registration (SessionStart) and creates the db if needed; otherwise this
/// stays a cheap no-op when there is no db yet. Best-effort: any failure
/// means "no messages".
pub(crate) fn sync(
    agent: &str,
    session: &HookSession,
    register: bool,
    policy: Delivery,
    surface: MailSurface,
) -> Vec<Message> {
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
    let cwd = session.cwd.as_deref();
    let take = takes_mail_on(agent, surface);
    sync_with_take(&conn, agent, &key, cwd, register, policy, now(), take)
        .or_else(|_| {
            // A table not created yet on a db an older binary made.
            let conn = crate::db::open()?;
            sync_with_take(&conn, agent, &key, cwd, register, policy, now(), take)
        })
        .unwrap_or_default()
}

pub(crate) fn sync_with(
    conn: &rusqlite::Connection,
    agent: &str,
    key: &str,
    cwd: Option<&str>,
    register: bool,
    policy: Delivery,
    now: i64,
) -> rusqlite::Result<Vec<Message>> {
    sync_with_take(
        conn,
        agent,
        key,
        cwd,
        register,
        policy,
        now,
        host_injects_context(agent),
    )
}

pub(crate) fn sync_with_take(
    conn: &rusqlite::Connection,
    _agent: &str,
    key: &str,
    cwd: Option<&str>,
    register: bool,
    policy: Delivery,
    now: i64,
    take: bool,
) -> rusqlite::Result<Vec<Message>> {
    identity::touch_hook_session(conn, key, cwd, register, now)?;
    // A submitted prompt starts a turn; a session start (`register`) is a
    // session idling at its prompt, not a turn (spec §5.4).
    if policy == Delivery::TurnStart && !register {
        crate::sessions::set_busy(conn, key, true, now)?;
    }
    let taken = if !take || !messages::has_undelivered(conn, key)? {
        vec![]
    } else {
        let markers = markers_for(conn, key, policy)?;
        messages::take_undelivered_where(conn, key, messages::MAX_BATCH, now, &markers)?
    };
    // A Stop that delivers mail blocks the stop, so the turn goes on; only a
    // Stop with nothing to deliver really ends it.
    if policy == Delivery::TurnEnd && taken.is_empty() {
        crate::sessions::set_busy(conn, key, false, now)?;
    }
    Ok(taken)
}

/// Model-visible context JSON for a host. Claude/Codex use nested
/// `hookSpecificOutput.additionalContext`; Cursor uses flat
/// `additional_context` (cursor.com/docs/hooks).
pub(crate) fn context_output(agent: &str, event: &str, context: &str) -> Value {
    if agent == "cursor" {
        json!({ "additional_context": context })
    } else {
        json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "additionalContext": context,
            }
        })
    }
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
        let key = identity::hook_key(agent, sid);
        if crate::sessions::end(&conn, &key, now()).is_err()
            // A column not added yet on a db an older binary made.
            && let Ok(conn) = crate::db::open()
        {
            let _ = crate::sessions::end(&conn, &key, now());
        }
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

/// Stop output: Claude/Codex block the stop with the messages as the
/// reason; Cursor auto-submits them as `followup_message` (its stop schema
/// has no decision/reason fields). Nothing pending -> let it stop.
pub(crate) fn stop_output(agent: &str, msgs: &[Message]) -> Option<Value> {
    (!msgs.is_empty()).then(|| {
        let text = messages::format_delivery(msgs);
        if agent == "cursor" {
            json!({ "followup_message": text })
        } else {
            json!({
                "decision": "block",
                "reason": text,
            })
        }
    })
}

/// Team messages replayed to a session joining the team (spec §5.5).
pub(crate) const REPLAY_LIMIT: usize = 10;

/// Context block for a fresh team member: what the team said before it
/// joined. Reuses the delivery envelope so the sender attribution and
/// escaping are identical to live mail.
pub(crate) fn format_replay(team: &str, msgs: &[Message]) -> Option<String> {
    if msgs.is_empty() {
        return None;
    }
    let mut out = format!(
        "agentflare: last {} message(s) sent to team:{team} — history, already seen by the team; \
         do not reply to these. New mail arrives separately.",
        msgs.len()
    );
    // Drop format_delivery's own header (its first line) and keep the envelopes.
    let rendered = messages::format_delivery(msgs);
    if let Some((_, envelopes)) = rendered.split_once('\n') {
        out.push('\n');
        out.push_str(envelopes);
    }
    Some(out)
}

/// The SessionStart replay for this process's team, if any. Best-effort:
/// no team (a dispatched job never has one), no db, or a db without the
/// table yet all mean no replay.
pub(crate) fn team_replay_block() -> Option<String> {
    let team = identity::member_team(identity::team_name())?;
    let conn = messages::open_fast()?;
    let msgs = messages::recent(&conn, &format!("team:{team}"), REPLAY_LIMIT).ok()?;
    format_replay(&team, &msgs)
}

/// `agentflare hook stop`.
pub fn stop(agent: &str) {
    let Some(input) = crate::hook::read_stdin_or_skip("Stop") else {
        return;
    };
    let msgs = sync(
        agent,
        &parse_session(&input),
        false,
        Delivery::TurnEnd,
        MailSurface::Stop,
    );
    if let Some(out) = stop_output(agent, &msgs) {
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

    fn seed(c: &rusqlite::Connection, to: &str, marker: &str, n: usize) {
        for i in 0..n {
            messages::send_marked(
                c,
                "codex:peer",
                to,
                &format!("{marker} {i}"),
                None,
                marker,
                100 + i as i64,
                no_item,
            )
            .unwrap();
        }
    }

    #[test]
    fn mid_turn_takes_important_never_fyi_and_status_only_in_batches() {
        let c = conn();
        let key = "claude-code:s1";
        seed(&c, key, "fyi", 2);
        seed(&c, key, "important", 1);
        seed(&c, key, "status", 2);
        let got = sync_with(&c, "claude-code", key, None, true, Delivery::MidTurn, 200).unwrap();
        let markers: Vec<&str> = got.iter().map(|m| m.marker.as_str()).collect();
        assert_eq!(markers, vec!["important"]);
        seed(&c, key, "status", 1); // now 3 status pending
        let got = sync_with(&c, "claude-code", key, None, false, Delivery::MidTurn, 201).unwrap();
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|m| m.marker == "status"));
        // FYI still waiting for a turn start.
        assert_eq!(messages::count_undelivered(&c, key).unwrap(), 2);
        let got = sync_with(
            &c,
            "claude-code",
            key,
            None,
            false,
            Delivery::TurnStart,
            202,
        )
        .unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn busy_clears_on_the_stop_that_delivers_nothing() {
        let c = conn();
        let key = "claude-code:s3";
        let busy = |c: &rusqlite::Connection| crate::sessions::get(c, key).unwrap().unwrap().busy;
        sync_with(&c, "claude-code", key, None, true, Delivery::TurnStart, 99).unwrap();
        assert!(!busy(&c), "a session start is not a turn");
        sync_with(
            &c,
            "claude-code",
            key,
            None,
            false,
            Delivery::TurnStart,
            100,
        )
        .unwrap();
        assert!(busy(&c), "a submitted prompt marks the session busy");
        sync_with(&c, "claude-code", key, None, false, Delivery::MidTurn, 101).unwrap();
        assert!(busy(&c));
        seed(&c, key, "important", 1);
        let got = sync_with(&c, "claude-code", key, None, false, Delivery::TurnEnd, 102).unwrap();
        assert_eq!(got.len(), 1);
        assert!(busy(&c), "a blocked stop keeps the turn running");
        let got = sync_with(&c, "claude-code", key, None, false, Delivery::TurnEnd, 103).unwrap();
        assert!(got.is_empty());
        assert!(!busy(&c), "a clean stop clears busy");
    }

    #[test]
    fn stop_lets_the_agent_stop_when_only_fyi_is_pending() {
        let c = conn();
        let key = "claude-code:s2";
        seed(&c, key, "fyi", 3);
        let got = sync_with(&c, "claude-code", key, None, true, Delivery::TurnEnd, 200).unwrap();
        assert!(got.is_empty());
        assert!(stop_output("claude-code", &got).is_none());
        seed(&c, key, "status", 1);
        let got = sync_with(&c, "claude-code", key, None, false, Delivery::TurnEnd, 201).unwrap();
        assert_eq!(got.len(), 1);
        assert!(stop_output("claude-code", &got).is_some());
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
        if identity::job_owner().is_some() {
            return; // a dispatched job's row only ever gets `last_seen_at`
        }
        let c = conn();
        let key = "claude-code:s1";
        assert!(
            sync_with(
                &c,
                "claude-code",
                key,
                Some("/repo"),
                true,
                Delivery::TurnStart,
                10
            )
            .unwrap()
            .is_empty()
        );
        let s = sessions::get(&c, key).unwrap().unwrap();
        assert_eq!(s.cwd.as_deref(), Some("/repo"));

        messages::send(&c, "codex:x", key, "please rebase", None, 11, no_item).unwrap();
        let got = sync_with(&c, "claude-code", key, None, false, Delivery::TurnStart, 12).unwrap();
        assert_eq!(got.len(), 1);
        assert!(
            sync_with(&c, "claude-code", key, None, false, Delivery::TurnStart, 13)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn sync_takes_on_cursor_when_the_surface_can_inject() {
        let c = conn();
        messages::send(&c, "a:1", "cursor:s", "hi", None, 1, no_item).unwrap();
        assert_eq!(
            sync_with_take(
                &c,
                "cursor",
                "cursor:s",
                None,
                true,
                Delivery::TurnStart,
                2,
                true,
            )
            .unwrap()
            .len(),
            1
        );
        assert!(!messages::has_undelivered(&c, "cursor:s").unwrap());
    }

    #[test]
    fn sync_does_not_take_on_cursor_surfaces_without_inject_fields() {
        let c = conn();
        messages::send(&c, "a:1", "cursor:s", "hi", None, 1, no_item).unwrap();
        assert!(
            sync_with_take(
                &c,
                "cursor",
                "cursor:s",
                None,
                false,
                Delivery::TurnStart,
                2,
                false,
            )
            .unwrap()
            .is_empty(),
            "beforeSubmitPrompt / allow-path preToolUse must not take"
        );
        assert!(messages::has_undelivered(&c, "cursor:s").unwrap());
    }

    #[test]
    fn cursor_stop_output_uses_followup_message() {
        assert!(stop_output("cursor", &[]).is_none());
        let out = stop_output("cursor", &[msg(3)]).unwrap();
        assert!(out.get("decision").is_none());
        assert!(out["followup_message"].as_str().unwrap().contains("id=3"));
    }

    #[test]
    fn cursor_context_output_is_flat_additional_context() {
        let out = context_output("cursor", "SessionStart", "hello");
        assert_eq!(out["additional_context"], "hello");
        assert!(out.get("hookSpecificOutput").is_none());
        let claude = context_output("claude-code", "SessionStart", "hello");
        assert_eq!(claude["hookSpecificOutput"]["additionalContext"], "hello");
    }

    #[test]
    fn parse_session_reads_cursor_workspace_roots_as_cwd() {
        let s = parse_session(r#"{"conversation_id":"c1","workspace_roots":["/repo"]}"#);
        assert_eq!(
            (s.session_id.as_deref(), s.cwd.as_deref()),
            (Some("c1"), Some("/repo"))
        );
    }

    #[test]
    fn takes_mail_on_cursor_only_for_inject_surfaces() {
        assert!(takes_mail_on("cursor", MailSurface::SessionStart));
        assert!(takes_mail_on("cursor", MailSurface::PostToolUse));
        assert!(takes_mail_on("cursor", MailSurface::Stop));
        assert!(!takes_mail_on("cursor", MailSurface::BeforeSubmitPrompt));
        assert!(!takes_mail_on("cursor", MailSurface::PreToolUse));
        assert!(takes_mail_on(
            "claude-code",
            MailSurface::BeforeSubmitPrompt
        ));
        assert!(!takes_mail_on("windsurf", MailSurface::Stop));
    }

    #[test]
    fn codex_hook_delivers_pending_messages() {
        let c = conn();
        messages::send(&c, "a:1", "codex:s", "hi", None, 1, no_item).unwrap();
        assert_eq!(
            sync_with(&c, "codex", "codex:s", None, true, Delivery::TurnStart, 2)
                .unwrap()
                .len(),
            1
        );
        assert!(!messages::has_undelivered(&c, "codex:s").unwrap());
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
            marker: "important".into(),
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
        assert!(ctx.contains("<agentflare-message from=\"codex:x\" id=7 marker=\"important\">"));
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
    fn format_replay_marks_history_as_already_read() {
        assert!(format_replay("alpha", &[]).is_none());
        let m = msg(1);
        let out = format_replay("alpha", std::slice::from_ref(&m)).unwrap();
        assert!(out.starts_with("agentflare: last 1 message(s) sent to team:alpha"));
        assert!(out.contains("history, already seen by the team; do not reply"));
        assert!(out.contains("<agentflare-message from=\"codex:x\""));
        assert!(
            !out.contains("NOT from your user"),
            "delivery header dropped"
        );
    }

    #[test]
    fn recent_returns_the_last_n_oldest_first() {
        let c = conn();
        for i in 0..12 {
            messages::send_marked(
                &c,
                "codex:peer",
                "claude-code:h",
                &format!("m{i}"),
                None,
                "fyi",
                100 + i,
                no_item,
            )
            .unwrap();
        }
        let got = messages::recent(&c, "claude-code:h", REPLAY_LIMIT).unwrap();
        assert_eq!(got.len(), 10);
        assert_eq!(got.first().unwrap().body, "m2");
        assert_eq!(got.last().unwrap().body, "m11");
    }

    #[test]
    fn recent_counts_a_team_message_once_however_many_members_got_it() {
        let c = conn();
        for key in ["claude-code:a1", "claude-code:a2", "codex:c1"] {
            let touch = sessions::Touch {
                key,
                team: Some("alpha"),
                ..Default::default()
            };
            sessions::touch(&c, &touch, 100).unwrap();
        }
        for i in 0..12 {
            let sent = messages::send_marked(
                &c,
                "claude-code:a1",
                "team:alpha",
                &format!("m{i}"),
                None,
                "important",
                100 + i,
                no_item,
            )
            .unwrap();
            assert_eq!(sent.recipients.len(), 2, "one row per other member");
        }
        let got = messages::recent(&c, "team:alpha", REPLAY_LIMIT).unwrap();
        let bodies: Vec<String> = got.into_iter().map(|m| m.body).collect();
        let last_ten: Vec<String> = (2..12).map(|i| format!("m{i}")).collect();
        assert_eq!(bodies, last_ten);
    }

    #[test]
    fn end_ends_the_session_on_a_db_an_older_binary_made() {
        if identity::job_owner().is_some() {
            return; // a dispatched job's session is never ended by its hooks
        }
        crate::paths::test_support::with_temp_home(|| {
            // A db from before `team`/`busy` existed, not yet opened (so not
            // yet migrated) by this binary: SessionEnd is its first hook.
            let path = crate::db::agentflare_db_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let old = rusqlite::Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE agent_sessions (
                    key TEXT PRIMARY KEY, agent TEXT NOT NULL, name TEXT, item_id TEXT, cwd TEXT,
                    host TEXT NOT NULL, pid INTEGER, started_at INTEGER NOT NULL,
                    last_seen_at INTEGER NOT NULL, ended_at INTEGER);
                 INSERT INTO agent_sessions (key,agent,host,started_at,last_seen_at)
                    VALUES ('claude-code:s9','claude-code','h',1,1);",
            )
            .unwrap();
            drop(old);
            let session = HookSession {
                session_id: Some("s9".into()),
                cwd: None,
            };
            end("claude-code", &session);
            let c = crate::db::open().unwrap();
            let s = sessions::get(&c, "claude-code:s9").unwrap().unwrap();
            assert!(s.ended_at.is_some(), "an ended session must not stay live");
        });
    }

    #[test]
    fn stop_output_blocks_only_with_messages() {
        assert!(stop_output("claude-code", &[]).is_none());
        let out = stop_output("claude-code", &[msg(3)]).unwrap();
        assert_eq!(out["decision"], "block");
        assert!(out["reason"].as_str().unwrap().contains("id=3"));
    }
}
