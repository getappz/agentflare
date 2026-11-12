use super::super::{StallConfig, run_captured_with_completion};
use super::*;
use crate::agent_launch::acp::{AcpCore, unattended_approver};
use serde_json::{Value, json};
use std::io::BufRead;
use std::process::Command;
use std::time::Duration;

const FIXTURE: &str = "agent_launch::stream::tests::fake_acp_agent";

/// A tiny ACP agent (this test binary re-run as the fixture, so it works on
/// every platform): opens session `fake-1`, and for the prompt streams a
/// reply, asks to run `git push`, reports the answer, ends the turn, then
/// keeps its stdin open like a real ACP server.
#[test]
#[ignore = "subprocess fixture for the ACP process test"]
fn fake_acp_agent() {
    assert_eq!(
        std::env::var("AGENTFLARE_TEST_ACP").as_deref(),
        Ok("agent"),
        "fixture must be launched by its process test"
    );
    let send = |v: Value| println!("{v}");
    let update = |u: Value| {
        send(json!({"jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": "fake-1", "update": u}}))
    };
    let mut prompt_id = None;
    for line in std::io::stdin().lock().lines() {
        let msg: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = msg["id"].clone();
        match msg["method"].as_str() {
            Some("initialize") => send(json!({"jsonrpc": "2.0", "id": id,
                "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": false}}})),
            Some("session/new") => {
                send(json!({"jsonrpc": "2.0", "id": id, "result": {"sessionId": "fake-1"}}))
            }
            Some("session/prompt") => {
                prompt_id = Some(id);
                update(json!({"sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "pushing"}}));
                update(json!({"sessionUpdate": "tool_call", "toolCallId": "t1",
                    "title": "git push", "kind": "execute", "status": "pending"}));
                send(
                    json!({"jsonrpc": "2.0", "id": 100, "method": "session/request_permission",
                    "params": {"sessionId": "fake-1",
                        "toolCall": {"toolCallId": "t1", "kind": "execute", "rawInput": {"command": "git push"}},
                        "options": [{"optionId": "ok", "kind": "allow_once", "name": "Allow"},
                                    {"optionId": "no", "kind": "reject_once", "name": "Reject"}]}}),
                );
            }
            // The permission answer.
            None => {
                let picked = msg["result"]["outcome"]["optionId"]
                    .as_str()
                    .unwrap_or("none");
                update(
                    json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1",
                    "status": "failed"}),
                );
                update(json!({"sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": format!("permission: {picked}")}}));
                send(json!({"jsonrpc": "2.0", "id": prompt_id.take(),
                    "result": {"stopReason": "end_turn"}}));
            }
            Some(_) => {}
        }
    }
}

#[test]
fn an_acp_agent_is_driven_over_stdin_and_its_turn_ends_the_run() {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--ignored", "--exact", FIXTURE, "--nocapture"])
        .env("AGENTFLARE_TEST_ACP", "agent");
    let core = AcpCore::new(std::env::temp_dir(), "push it".into(), None, None);
    let started = Instant::now();
    let out = run_captured_with_completion(
        cmd,
        Duration::from_secs(60),
        Duration::from_secs(30),
        None,
        None,
        StallConfig::from_env(),
        Some(Stream::Acp(Box::new(core), unattended_approver())),
    )
    .unwrap();
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the ended turn must end the lingering server: {:?}",
        started.elapsed()
    );
    let turn = out.turn.expect("streamed");
    assert!(turn.terminal);
    assert_eq!(turn.tools.len(), 1);
    assert!(turn.tools[0].finished);
    let reply = turn.into_reply(&out.stdout);
    assert_eq!(reply.session_id.as_deref(), Some("fake-1"));
    assert_eq!(reply.subtype.as_deref(), Some("success"));
    // `git push` is network: the unattended policy rejects it.
    assert_eq!(reply.text, "permission: no");
}

/// One real ACP turn against the installed opencode (default model, a
/// trivial prompt). Live and billed, so run by hand:
/// `cargo test -p agentflare --bin agentflare live_opencode_acp_turn -- --ignored --nocapture`.
#[test]
#[ignore = "live: needs opencode installed and signed in"]
fn live_opencode_acp_turn() {
    let mut cmd = Command::new("opencode");
    cmd.arg("acp");
    let cwd = std::env::temp_dir();
    cmd.current_dir(&cwd);
    let core = AcpCore::new(cwd, "Reply with exactly: pong".into(), None, None);
    let started = Instant::now();
    let out = run_captured_with_completion(
        cmd,
        Duration::from_secs(300),
        Duration::from_secs(120),
        None,
        None,
        StallConfig::from_env(),
        Some(Stream::Acp(Box::new(core), unattended_approver())),
    )
    .unwrap();
    let turn = out.turn.expect("streamed");
    let (terminal, tools) = (turn.terminal, turn.tools.len());
    let reply = turn.into_reply(&out.stdout);
    println!(
        "success={} terminal={terminal} tools={tools} elapsed={:?} timed_out={} reply={reply:?}",
        out.success,
        started.elapsed(),
        out.timed_out
    );
    assert!(out.success && terminal, "stderr: {}", out.stderr);
    assert_eq!(reply.text.trim(), "pong");
    assert!(reply.session_id.is_some());
}
