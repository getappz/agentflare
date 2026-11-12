use super::*;
use agent_registry::ReplyFormat;

fn core(resume: Option<&str>, model: Option<&str>) -> AcpCore {
    AcpCore::new(
        PathBuf::from("/work"),
        "do it".into(),
        resume.map(str::to_string),
        model.map(str::to_string),
    )
}

/// The JSON of each `Send`.
fn sent(outs: &[Out]) -> Vec<Value> {
    outs.iter()
        .filter_map(|o| match o {
            Out::Send(line) => Some(serde_json::from_str(line).unwrap()),
            Out::Decide { .. } => None,
        })
        .collect()
}

fn feed(core: &mut AcpCore, turn: &mut TurnState, msg: Value) -> Vec<Out> {
    core.line(msg.to_string().as_bytes(), turn)
}

fn update(update: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": update}})
}

/// Drives `core` from `initialize` to an open session `s1` with its
/// prompt (request id 3) sent.
fn open(core: &mut AcpCore, turn: &mut TurnState) {
    core.start();
    feed(core, turn, json!({"jsonrpc": "2.0", "id": 1, "result": {}}));
    let prompt = sent(&feed(
        core,
        turn,
        json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "s1"}}),
    ));
    assert_eq!(prompt[0]["method"], "session/prompt");
    assert_eq!(prompt[0]["id"], 3);
}

#[test]
fn init_advertises_no_fs_or_terminal_and_opens_a_new_session() {
    let mut turn = TurnState::new(ReplyFormat::AcpUpdates);
    let mut c = core(None, None);
    let init = sent(&c.start()).remove(0);
    assert_eq!(init["params"]["protocolVersion"], 1);
    assert_eq!(
        init["params"]["clientCapabilities"],
        json!({"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false})
    );
    let new = sent(&feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 1, "result": {"agentCapabilities": {}}}),
    ))
    .remove(0);
    assert_eq!(new["method"], "session/new");
    assert_eq!(new["params"]["mcpServers"], json!([]));
    let prompt = sent(&feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "s1"}}),
    ))
    .remove(0);
    assert_eq!(prompt["method"], "session/prompt");
    assert_eq!(
        prompt["params"],
        json!({"sessionId": "s1", "prompt": [{"type": "text", "text": "do it"}]})
    );
    assert_eq!(turn.reply.session_id.as_deref(), Some("s1"));
}

#[test]
fn a_routed_model_is_selected_before_the_prompt_unless_already_current() {
    let mut turn = TurnState::new(ReplyFormat::AcpUpdates);
    let mut c = core(None, Some("p/m"));
    c.start();
    feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
    );
    let set = sent(&feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "s1",
            "configOptions": [{"id": "model", "currentValue": "other"}]}}),
    ))
    .remove(0);
    assert_eq!(set["method"], "session/set_config_option");
    assert_eq!(
        set["params"],
        json!({"sessionId": "s1", "configId": "model", "value": "p/m"})
    );
    let prompt = sent(&feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 3, "result": {}}),
    ));
    assert_eq!(prompt[0]["method"], "session/prompt");

    let mut c = core(None, Some("p/m"));
    c.start();
    feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
    );
    let prompt = sent(&feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "s1",
            "configOptions": [{"id": "model", "currentValue": "p/m"}]}}),
    ));
    assert_eq!(prompt[0]["method"], "session/prompt");
}

#[test]
fn updates_stream_the_last_message_and_tool_calls_into_the_turn() {
    let mut turn = TurnState::new(ReplyFormat::AcpUpdates);
    let mut c = core(None, None);
    open(&mut c, &mut turn);
    let chunk = |t: &str| {
        update(
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": t}}),
        )
    };
    for msg in [
        chunk("Let me "),
        chunk("look."),
        update(
            json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read a.rs", "kind": "read", "status": "pending"}),
        ),
        update(
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"}),
        ),
        update(
            json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}}),
        ),
        chunk("Done"),
        chunk("."),
    ] {
        assert!(feed(&mut c, &mut turn, msg).is_empty());
    }
    assert_eq!(turn.reply.text, "Done.");
    assert_eq!(turn.tools.len(), 1);
    assert_eq!(turn.tools[0].name, "Read a.rs");
    assert!(turn.tools[0].finished);
    assert!(!turn.terminal);

    feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 3, "result": {"stopReason": "end_turn"}}),
    );
    assert!(turn.terminal);
    let reply = turn.into_reply("raw json-rpc transcript");
    assert_eq!(reply.text, "Done.");
    assert_eq!(reply.subtype.as_deref(), Some("success"));
    assert!(!reply.is_error);
    assert_eq!(reply.session_id.as_deref(), Some("s1"));
}

#[test]
fn permission_requests_are_decided_in_policy_terms_and_answered_once() {
    let mut turn = TurnState::new(ReplyFormat::AcpUpdates);
    let mut c = core(None, None);
    open(&mut c, &mut turn);
    let options = json!([
        {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
        {"optionId": "always", "name": "Always", "kind": "allow_always"},
        {"optionId": "no", "name": "Reject", "kind": "reject_once"},
    ]);
    let request = |id: u64, call: Value| {
        json!({"jsonrpc": "2.0", "id": id, "method": "session/request_permission",
            "params": {"sessionId": "s1", "toolCall": call, "options": options}})
    };
    let outs = feed(
        &mut c,
        &mut turn,
        request(
            7,
            json!({"toolCallId": "t1", "kind": "execute", "rawInput": {"command": ["git", "push"]}}),
        ),
    );
    let [Out::Decide { ask, tool, input }] = &outs[..] else {
        panic!("{outs:?}")
    };
    assert_eq!(
        (tool.as_str(), input),
        ("Bash", &json!({"command": "git push"}))
    );
    assert!(!allowed(AutonomyTier::Full, tool, input), "network");
    let reply = sent(&c.answer(*ask, false)).remove(0);
    assert_eq!(
        reply,
        json!({"jsonrpc": "2.0", "id": 7, "result": {"outcome": {"outcome": "selected", "optionId": "no"}}})
    );
    assert!(c.answer(*ask, true).is_empty(), "answered once");

    let outs = feed(
        &mut c,
        &mut turn,
        request(
            8,
            json!({"toolCallId": "t2", "kind": "edit", "locations": [{"path": "/work/a.rs"}]}),
        ),
    );
    let [Out::Decide { ask, tool, input }] = &outs[..] else {
        panic!("{outs:?}")
    };
    assert_eq!(
        (tool.as_str(), input),
        ("Write", &json!({"file_path": "/work/a.rs"}))
    );
    assert!(allowed(AutonomyTier::Full, tool, input));
    assert!(!allowed(AutonomyTier::Supervised, tool, input));
    assert_eq!(
        sent(&c.answer(*ask, true))[0]["result"]["outcome"]["optionId"],
        "yes"
    );
    for (kind, tool) in [("read", "Read"), ("fetch", "WebFetch")] {
        let outs = feed(&mut c, &mut turn, request(9, json!({"kind": kind})));
        assert!(matches!(&outs[..], [Out::Decide { tool: t, .. }] if t == tool));
    }
}

#[test]
fn unoffered_agent_requests_get_method_not_found() {
    let mut turn = TurnState::new(ReplyFormat::AcpUpdates);
    let mut c = core(None, None);
    open(&mut c, &mut turn);
    let reply = sent(&feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": "r1", "method": "fs/read_text_file", "params": {}}),
    ))
    .remove(0);
    assert_eq!(reply["id"], "r1");
    assert_eq!(reply["error"]["code"], -32601);
}

#[test]
fn an_error_response_fails_the_turn_with_its_message() {
    let mut turn = TurnState::new(ReplyFormat::AcpUpdates);
    let mut c = core(None, None);
    c.start();
    let outs = feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "auth required"}}),
    );
    assert!(outs.is_empty());
    assert!(turn.terminal);
    let reply = turn.into_reply("");
    assert!(reply.is_error);
    assert_eq!(reply.subtype.as_deref(), Some("error"));
    assert_eq!(reply.text, "initialize failed: auth required");
}

#[test]
fn a_loaded_session_drops_its_replayed_history() {
    let mut turn = TurnState::new(ReplyFormat::AcpUpdates);
    let mut c = core(Some("old"), None);
    c.start();
    let load = sent(&feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 1, "result": {"agentCapabilities": {"loadSession": true}}}),
    ))
    .remove(0);
    assert_eq!(load["method"], "session/load");
    assert_eq!(load["params"]["sessionId"], "old");
    feed(
        &mut c,
        &mut turn,
        update(
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "old reply"}}),
        ),
    );
    feed(
        &mut c,
        &mut turn,
        update(json!({"sessionUpdate": "tool_call", "toolCallId": "old-t", "title": "x"})),
    );
    assert_eq!(turn.reply.text, "");
    assert!(turn.tools.is_empty());
    let prompt = sent(&feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 2, "result": null}),
    ))
    .remove(0);
    assert_eq!(prompt["params"]["sessionId"], "old");
    assert_eq!(turn.reply.session_id.as_deref(), Some("old"));
}

#[test]
fn a_refused_or_unsupported_load_starts_a_new_session() {
    let mut turn = TurnState::new(ReplyFormat::AcpUpdates);
    let mut c = core(Some("old"), None);
    c.start();
    let new = sent(&feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 1, "result": {"agentCapabilities": {}}}),
    ));
    assert_eq!(new[0]["method"], "session/new");

    let mut c = core(Some("old"), None);
    c.start();
    feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 1, "result": {"agentCapabilities": {"loadSession": true}}}),
    );
    let new = sent(&feed(
        &mut c,
        &mut turn,
        json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32002, "message": "not found"}}),
    ));
    assert_eq!(new[0]["method"], "session/new");
    assert!(!turn.terminal);
}

#[test]
fn stop_reasons_map_onto_the_cli_result_subtypes() {
    for (reason, subtype, is_error) in [
        ("end_turn", "success", false),
        ("max_tokens", "error_max_tokens", false),
        ("max_turn_requests", "error_max_turns", false),
        ("refusal", "refusal", true),
        ("cancelled", "cancelled", true),
    ] {
        let mut turn = TurnState::new(ReplyFormat::AcpUpdates);
        let mut c = core(None, None);
        open(&mut c, &mut turn);
        feed(
            &mut c,
            &mut turn,
            json!({"jsonrpc": "2.0", "id": 3, "result": {"stopReason": reason}}),
        );
        assert!(turn.terminal, "{reason}");
        assert_eq!(turn.reply.subtype.as_deref(), Some(subtype));
        assert_eq!(turn.reply.is_error, is_error, "{reason}");
    }
}

#[test]
fn session_args_read_resume_and_model_from_a_print_argv() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        session_args(
            Agent::Opencode,
            &v(&["--auto", "--session", "s9", "--model", "p/m"])
        ),
        (Some("s9".into()), Some("p/m".into()))
    );
    assert_eq!(
        session_args(Agent::Opencode, &v(&["--session=s9", "--format", "json"])),
        (Some("s9".into()), None)
    );
    assert_eq!(session_args(Agent::Opencode, &[]), (None, None));
}

#[test]
fn acp_is_opt_in_and_the_cli_path_is_the_default() {
    assert!(!opted_in("opencode", None), "unset: print-mode CLI");
    assert!(!opted_in("opencode", Some("")));
    assert!(!opted_in("opencode", Some("codex")));
    assert!(opted_in("opencode", Some("opencode")));
    assert!(opted_in("opencode", Some("codex, opencode")));
}
