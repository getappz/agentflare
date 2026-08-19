# SDD-Loop Session Resume Implementation Plan

**Goal:** Wire up `WorkItemData::session_id` (an already-existing but currently-dead field) so `work_item_pipeline.rs`'s SDD loop resumes the underlying claude-code / cursor-agent provider session across fix-rounds and review cycles instead of respawning a cold, full-context process on every turn.

**Architecture:** `agent_launch::run_headless` gains an opt-in `request_json: bool` param; when set (and the target agent is `ClaudeCode`/`Cursor`), it requests `--output-format json` and parses `result`/`session_id`/`total_cost_usd` out of the reply instead of treating stdout as plain text. `work_item_pipeline.rs`'s `real_agent_send_hook` turns this on and smuggles the captured session id back through the existing plain-string `SendMessage` reply channel using a private marker — the same idiom this file already uses for `REVIEW_ISSUES_MARKER`/`REVIEW_APPROVED_MARKER` — so `flare_workflow::json::{SendMessage, StepInvocation}` (a shared crate type explicitly flagged in `ROLLBACK_COMPENSATION_DESIGN.md` as out of scope for casual widening) is never touched. `build_sdd_loop_step` strips the marker, stores the session id per dispatched agent in `WorkItemData::agent_sessions`, and passes `--resume <id>` as an ordinary extra CLI arg (`StepInvocation::args`, already a plain `Vec<String>`) on the next turn for that same agent.

**Tech Stack:** Rust, `serde_json` (already a dependency of the `agentflare` binary crate — no new deps), `flare_workflow` (untouched), `agent-registry`, `agentflare-jobs::sandbox` (untouched — this only changes argv content, not sandbox mount policy).

**Spec:** No separate spec doc. Scoped directly from a live architecture review of a Rust multi-agent-workflow-engine design note against this repo's actual `agent-registry`/`agentflare-jobs`/`flare-workflow`/`work_item_pipeline.rs`. The provider JSON schemas and `--resume` behavior below were verified empirically (real `cursor-agent`/`claude` CLI calls), not assumed:

```
$ cursor-agent -p --output-format json --force "reply with exactly the word: pong"
{"type":"result","subtype":"success","is_error":false,"duration_ms":7147,"duration_api_ms":7147,
 "result":"pong","session_id":"4e4bb132-b0ca-47cd-9522-23ca63211c70",
 "request_id":"...","usage":{"inputTokens":12983,"outputTokens":96,"cacheReadTokens":5957,"cacheWriteTokens":0}}

$ cursor-agent -p --output-format json --force --resume 4e4bb132-b0ca-47cd-9522-23ca63211c70 "what word did you just reply with?"
{"type":"result","subtype":"success","is_error":false, ...,
 "result":"pong","session_id":"4e4bb132-b0ca-47cd-9522-23ca63211c70", ...,
 "usage":{"inputTokens":152,"outputTokens":38, ...}}   # 12983 -> 152 input tokens on resume

$ claude -p --output-format json --dangerously-skip-permissions "reply with exactly the word: pong"
{"is_error":false, ..., "session_id":"8846d0f1-...", "total_cost_usd":0.1038312, ...,
 "type":"result", "result":"pong", ...}
```

Both CLIs share `type":"result"`, `result` (the text reply), `session_id`, and `is_error`. Only `claude` returns a top-level `total_cost_usd`; `cursor-agent` does not — `HeadlessReply::cost_usd` is `None` for cursor by design, not a bug.

## Global Constraints

- Do NOT change `flare_workflow::json::StepInvocation` or `SendMessage`'s type signatures — `crates/flare-workflow/ROLLBACK_COMPENSATION_DESIGN.md:305` explicitly calls widening `SendMessage` "a separate, larger surface change." Every task below stays inside `agent-registry`, `src/agent_launch.rs`, `src/agents.rs`, `src/workflow.rs`, and `src/work_item_pipeline.rs`.
- JSON-output/session support is scoped to `Agent::ClaudeCode` and `Agent::Cursor` only (the two verified above). Every other agent variant must keep behaving exactly as it does today — `json_output_args`/`resume_arg` return `None` for them, so no argv or parsing change reaches those code paths.
- `request_json` on `run_headless` is opt-in per call site, not automatic-by-agent-identity. `agent_launch.rs`'s own test suite reuses `Agent::ClaudeCode` against a `sh` stub binary (`run_headless_pipes_a_prompt_over_the_argv_length_limit_via_stdin`) — if JSON output were forced on by agent identity alone, that stub would receive `--output-format json` it doesn't understand and the test would break. Only `work_item_pipeline.rs`'s real dispatch hook passes `request_json: true`; `agents.rs`/`workflow.rs`/existing tests pass `false` and are otherwise unaffected.
- No new crate dependencies.

---

### Task 1: `agent-registry` — JSON output & resume flag mapping

**Files:**
- Modify: `crates/agent-registry/src/registry.rs:329-363` (right after `headless_args`/`autonomous_args`)

**Interfaces:**
- Consumes: `agent_registry::Agent` (existing enum, `crates/agent-registry/src/registry.rs:9-30`)
- Produces: `agent_registry::json_output_args(agent: Agent) -> Option<&'static [&'static str]>`, `agent_registry::resume_arg(agent: Agent) -> Option<&'static str>` — consumed by Task 2 and Task 4

- [ ] Step 1: Add the two functions:

```rust
/// CLI flags that switch an agent's headless print mode to structured JSON
/// output (`{"type":"result","result":"...","session_id":"...",...}`),
/// letting `agent_launch::run_headless` capture a provider session id and
/// (where the provider reports it) a cost instead of treating stdout as an
/// opaque text reply. `None` for every agent whose JSON schema hasn't been
/// verified — `run_headless` must fall back to plain-text parsing for those,
/// never guess a schema.
#[allow(dead_code)]
#[must_use]
pub fn json_output_args(agent: Agent) -> Option<&'static [&'static str]> {
    match agent {
        Agent::ClaudeCode | Agent::Cursor => Some(&["--output-format", "json"]),
        _ => None,
    }
}

/// The flag that resumes a prior session by id (e.g. `claude --resume
/// <session_id>`, `cursor-agent --resume <chatId>`), appended after
/// print-mode flags and before the prompt. `None` for agents with no known
/// resume flag.
#[allow(dead_code)]
#[must_use]
pub fn resume_arg(agent: Agent) -> Option<&'static str> {
    match agent {
        Agent::ClaudeCode | Agent::Cursor => Some("--resume"),
        _ => None,
    }
}
```

- [ ] Step 2: Add tests in the existing `#[cfg(test)]` module (append after `autonomous_args_maps_cursor_to_force`):

```rust
    #[test]
    fn json_output_args_maps_claude_and_cursor_only() {
        assert_eq!(json_output_args(Agent::ClaudeCode), Some(&["--output-format", "json"][..]));
        assert_eq!(json_output_args(Agent::Cursor), Some(&["--output-format", "json"][..]));
        assert_eq!(json_output_args(Agent::Opencode), None);
        assert_eq!(json_output_args(Agent::Codex), None);
    }

    #[test]
    fn resume_arg_maps_claude_and_cursor_only() {
        assert_eq!(resume_arg(Agent::ClaudeCode), Some("--resume"));
        assert_eq!(resume_arg(Agent::Cursor), Some("--resume"));
        assert_eq!(resume_arg(Agent::Opencode), None);
        assert_eq!(resume_arg(Agent::Codex), None);
    }
```

- [ ] Step 3: Run: `cargo test -p agent-registry json_output_args resume_arg` — expect PASS.
- [ ] Step 4: Commit: `feat(agent-registry): add json_output_args/resume_arg for claude-code and cursor`

---

### Task 2: `agent_launch.rs` — capture session id from JSON output

**Files:**
- Modify: `src/agent_launch.rs:343-449` (`HeadlessOutcome`, `run_headless`), `:876-920` (existing stub test)

**Interfaces:**
- Consumes: `agent_registry::json_output_args`, `agent_registry::resume_arg` (Task 1)
- Produces: `pub struct HeadlessReply { pub text: String, pub session_id: Option<String>, pub cost_usd: Option<f64> }`; `HeadlessOutcome::Ok(HeadlessReply)` (was `Ok(String)`); `run_headless(registry, agent, prompt, hard_cap, idle_timeout, extra_args, request_json: bool) -> HeadlessOutcome` (new trailing param)

- [ ] Step 1: Replace lines 343-356 with:

```rust
#[allow(dead_code)]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeadlessReply {
    pub text: String,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum HeadlessOutcome {
    Ok(HeadlessReply),
    UnknownAgent(String),
    NotHeadless(String),
    NotFound(String),
    Failed(String),
}
```

- [ ] Step 2: Add a private JSON parser above `run_headless` (before line 364):

```rust
fn parse_json_reply(stdout: &str) -> HeadlessReply {
    match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        Ok(value) => HeadlessReply {
            text: value.get("result").and_then(serde_json::Value::as_str).unwrap_or(stdout).to_string(),
            session_id: value.get("session_id").and_then(serde_json::Value::as_str).map(str::to_string),
            cost_usd: value.get("total_cost_usd").and_then(serde_json::Value::as_f64),
        },
        Err(_) => HeadlessReply { text: stdout.to_string(), session_id: None, cost_usd: None },
    }
}
```

- [ ] Step 3: Change `run_headless`'s signature (line 364-371) to add `request_json: bool` as the trailing parameter. Update the argv-building block (lines 389-394) to prepend `json_output_args(spec.id)`'s flags ahead of `extra_args` when `request_json` is true and the agent supports it:

```rust
    let mut full_args: Vec<String> = Vec::with_capacity(extra_args.len() + 2);
    if request_json && let Some(flags) = json_output_args(spec.id) {
        full_args.extend(flags.iter().map(|s| (*s).to_string()));
    }
    full_args.extend(extra_args.iter().cloned());
    let Some(argv) = headless_argv(spec.id, &binary, &full_args) else {
        return HeadlessOutcome::NotHeadless(format!("{} has no headless print mode", spec.display_name));
    };
```

Add `json_output_args` to the `agent_registry` import at the top of the file. Replace the success arm (`Ok(c) if c.success => HeadlessOutcome::Ok(c.stdout),`) with:

```rust
        Ok(c) if c.success => {
            if request_json && json_output_args(spec.id).is_some() {
                HeadlessOutcome::Ok(parse_json_reply(&c.stdout))
            } else {
                HeadlessOutcome::Ok(HeadlessReply { text: c.stdout, session_id: None, cost_usd: None })
            }
        }
```

- [ ] Step 4: Update every existing `run_headless(...)` test call (4 of them, around lines 831-920) to add a trailing `false`. In `run_headless_pipes_a_prompt_over_the_argv_length_limit_via_stdin`, also change `HeadlessOutcome::Ok(out) => assert_eq!(out.trim(), ...)` to `HeadlessOutcome::Ok(reply) => assert_eq!(reply.text.trim(), ...)`.

- [ ] Step 5: Add new tests (append to `#[cfg(test)] mod tests`):

```rust
    #[test]
    fn parse_json_reply_extracts_result_session_and_cost() {
        let reply = parse_json_reply(r#"{"type":"result","result":"pong","session_id":"abc-123","total_cost_usd":0.05}"#);
        assert_eq!(reply.text, "pong");
        assert_eq!(reply.session_id.as_deref(), Some("abc-123"));
        assert_eq!(reply.cost_usd, Some(0.05));
    }

    #[test]
    fn parse_json_reply_handles_missing_cost_usd() {
        let reply = parse_json_reply(r#"{"type":"result","result":"pong","session_id":"abc-123"}"#);
        assert_eq!(reply.text, "pong");
        assert_eq!(reply.session_id.as_deref(), Some("abc-123"));
        assert_eq!(reply.cost_usd, None);
    }

    #[test]
    fn parse_json_reply_falls_back_to_raw_text_on_malformed_json() {
        let reply = parse_json_reply("not json at all");
        assert_eq!(reply.text, "not json at all");
        assert_eq!(reply.session_id, None);
        assert_eq!(reply.cost_usd, None);
    }

    #[cfg(unix)]
    #[test]
    fn run_headless_with_request_json_parses_a_json_reply_from_the_child() {
        let reg = vec![AgentSpec {
            id: Agent::ClaudeCode,
            display_name: "claude-code",
            tier: Tier::Cli,
            binary_names: &["sh"],
            version_args: &[],
            package_manager: None,
            package_name: None,
        }];
        match run_headless(
            &reg, "claude-code", "hi",
            Duration::from_secs(10), Duration::from_secs(10),
            &["-c".to_string(), r#"echo '{"type":"result","result":"pong","session_id":"sess-xyz"}'"#.to_string()],
            true,
        ) {
            HeadlessOutcome::Ok(reply) => {
                assert_eq!(reply.text, "pong");
                assert_eq!(reply.session_id.as_deref(), Some("sess-xyz"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }
```

- [ ] Step 6: Run: `cargo test -p agentflare parse_json_reply run_headless` — expect all PASS.
- [ ] Step 7: Commit: `feat(agent-launch): capture session id and cost from JSON headless replies`

---

### Task 3: Update the other two `run_headless`/`HeadlessOutcome::Ok` call sites

**Files:**
- Modify: `src/agents.rs:229-253` (`cli_run_headless`)
- Modify: `src/workflow.rs:63-116` (`agent_send_hook`)

- [ ] Step 1: In `cli_run_headless`, add a trailing `false` to the `run_headless(...)` call and change `HeadlessOutcome::Ok(reply) => { print!("{reply}"); 0 }` to `HeadlessOutcome::Ok(reply) => { print!("{}", reply.text); 0 }`.
- [ ] Step 2: In `agent_send_hook` (`src/workflow.rs`), add a trailing `false` to its `run_headless(...)` call and change `HeadlessOutcome::Ok(reply) => Ok((reply, 0, 0))` to `HeadlessOutcome::Ok(reply) => Ok((reply.text, 0, 0))`.
- [ ] Step 3: Run: `cargo test -p agentflare cli_run_headless agent_send_hook` — expect all PASS.
- [ ] Step 4: Commit: `chore: adapt cli_run_headless and agent_send_hook to HeadlessReply`

---

### Task 4: Wire session resume into the SDD loop

**Files:**
- Modify: `src/work_item_pipeline.rs:43-64` (`WorkItemData`), `:227-416` (`build_sdd_loop_step`), `:569-607` (`real_agent_send_hook`)

- [ ] Step 1: Add a field to `WorkItemData` (after `last_report`):

```rust
    /// Provider session id last observed for each agent name dispatched in
    /// this run (implementer and judge/reviewer are usually different
    /// agents and get independent entries). Used to pass `--resume <id>` on
    /// that agent's next turn instead of respawning a cold, full-context
    /// session.
    #[serde(default)]
    pub agent_sessions: std::collections::HashMap<String, String>,
```

- [ ] Step 2: Add marker encode/decode helpers and `resume_args_for`, above `build_sdd_loop_step`, next to `REVIEW_ISSUES_MARKER`/`REVIEW_APPROVED_MARKER`:

```rust
/// Smuggles a captured provider session id back through the plain-string
/// reply channel `flare_workflow::json::SendMessage` returns, the same way
/// REVIEW_ISSUES_MARKER/REVIEW_APPROVED_MARKER above smuggle control
/// signals — chosen specifically so SendMessage's type never needs to
/// widen. Always stripped before a reply is stored in ctx.data or used to
/// build any further prompt — it never reaches a judge prompt, a PR
/// comment, or a human.
const SESSION_MARKER: &str = "\u{0}AGENTFLARE_SESSION:";

fn encode_session(reply: &str, session_id: Option<&str>) -> String {
    match session_id {
        Some(id) => format!("{reply}{SESSION_MARKER}{id}"),
        None => reply.to_string(),
    }
}

fn strip_session_marker(reply: &str) -> (String, Option<String>) {
    match reply.split_once(SESSION_MARKER) {
        Some((clean, id)) => (clean.to_string(), Some(id.to_string())),
        None => (reply.to_string(), None),
    }
}

fn resume_args_for(agent_name: &str, sessions: &std::collections::HashMap<String, String>) -> Vec<String> {
    let Some(agent) = agent_registry::agent_by_name(agent_name) else { return Vec::new(); };
    let Some(flag) = agent_registry::resume_arg(agent) else { return Vec::new(); };
    match sessions.get(agent_name) {
        Some(session_id) => vec![flag.to_string(), session_id.clone()],
        None => Vec::new(),
    }
}
```

- [ ] Step 3: In `real_agent_send_hook`, honor `inv.args` (currently discarded) and encode the session id into the returned reply:

```rust
fn real_agent_send_hook(
    timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
    extra_args: Vec<String>,
) -> flare_workflow::json::SendMessage {
    std::sync::Arc::new(move |inv: flare_workflow::json::StepInvocation| {
        let mut all_args = extra_args.clone();
        all_args.extend(inv.args.clone());
        let flare_workflow::json::StepInvocation { agent, prompt, .. } = inv;
        Box::pin(async move {
            let outcome = tokio::task::spawn_blocking(move || {
                crate::agent_launch::run_headless(
                    agent_registry::REGISTRY, &agent, &prompt, timeout, idle_timeout, &all_args, true,
                )
            })
            .await
            .map_err(|e| format!("agent task panicked: {e}"))?;
            match outcome {
                crate::agent_launch::HeadlessOutcome::Ok(reply) => Ok((
                    encode_session(&reply.text, reply.session_id.as_deref()), 0, 0,
                )),
                crate::agent_launch::HeadlessOutcome::UnknownAgent(e)
                | crate::agent_launch::HeadlessOutcome::NotHeadless(e)
                | crate::agent_launch::HeadlessOutcome::NotFound(e)
                | crate::agent_launch::HeadlessOutcome::Failed(e) => Err(e),
            }
        })
    })
}
```

- [ ] Step 4: In `build_sdd_loop_step`'s role dispatch (the block building `role_agent`/`role_prompt` then calling `send(StepInvocation::simple(role_agent, role_prompt))`), replace the send call and reply handling with:

```rust
                let role_invocation = flare_workflow::json::StepInvocation {
                    args: resume_args_for(&role_agent, &ctx.data.agent_sessions),
                    ..flare_workflow::json::StepInvocation::simple(role_agent.clone(), role_prompt)
                };
                let (raw_role_reply, in_tok, out_tok) = send(role_invocation)
                    .await
                    .map_err(|message| WorkflowError::StepFailed { step_id: StepId::new("sdd_loop"), message })?;
                ctx.input_tokens += in_tok;
                ctx.output_tokens += out_tok;

                let (role_reply, role_session_id) = strip_session_marker(&raw_role_reply);
                if let Some(id) = role_session_id {
                    ctx.data.agent_sessions.insert(role_agent.clone(), id.clone());
                    if role_agent == agent_name {
                        ctx.data.session_id = Some(id);
                    }
                }

                if let Some(issues) = role_reply.strip_prefix(REVIEW_ISSUES_MARKER) {
                    ctx.data.review_issues = Some(issues.trim().to_string());
                    ctx.data.last_report = None;
                } else if role_reply.trim() == REVIEW_APPROVED_MARKER {
                    ctx.data.review_issues = None;
                } else {
                    ctx.data.last_report = Some(role_reply.clone());
                }
```

- [ ] Step 5: Do the same for the judge dispatch immediately below it:

```rust
                let judge_prompt = build_judge_prompt(&ctx.data.tasks, ctx.data.current_task_index, &ctx.data.ledger, &role_reply);
                let judge_invocation = flare_workflow::json::StepInvocation {
                    args: resume_args_for(&judge_agent_name, &ctx.data.agent_sessions),
                    ..flare_workflow::json::StepInvocation::simple(judge_agent_name.clone(), judge_prompt)
                };
                let (raw_judge_reply, jin_tok, jout_tok) = send(judge_invocation)
                    .await
                    .map_err(|message| WorkflowError::StepFailed { step_id: StepId::new("sdd_loop"), message })?;
                ctx.input_tokens += jin_tok;
                ctx.output_tokens += jout_tok;

                let (judge_reply, judge_session_id) = strip_session_marker(&raw_judge_reply);
                if let Some(id) = judge_session_id {
                    ctx.data.agent_sessions.insert(judge_agent_name.clone(), id);
                }

                let decision = match parse_judge_decision(&judge_reply) {
```

(Rest of the function is unchanged.)

- [ ] Step 6: Add tests (append to `#[cfg(test)] mod tests`):

```rust
    #[test]
    fn strip_session_marker_recovers_id_and_leaves_reply_clean() {
        let encoded = encode_session("did the thing", Some("sess-42"));
        let (clean, id) = strip_session_marker(&encoded);
        assert_eq!(clean, "did the thing");
        assert_eq!(id.as_deref(), Some("sess-42"));
    }

    #[test]
    fn strip_session_marker_is_a_noop_without_a_marker() {
        let (clean, id) = strip_session_marker("plain reply, no marker");
        assert_eq!(clean, "plain reply, no marker");
        assert_eq!(id, None);
    }

    #[test]
    fn resume_args_for_empty_when_no_prior_session() {
        let sessions = std::collections::HashMap::new();
        assert_eq!(resume_args_for("claude-code", &sessions), Vec::<String>::new());
    }

    #[test]
    fn resume_args_for_empty_for_an_agent_with_no_resume_flag() {
        let mut sessions = std::collections::HashMap::new();
        sessions.insert("opencode".to_string(), "sess-1".to_string());
        assert_eq!(resume_args_for("opencode", &sessions), Vec::<String>::new());
    }

    #[test]
    fn resume_args_for_builds_the_resume_flag_when_a_session_is_known() {
        let mut sessions = std::collections::HashMap::new();
        sessions.insert("claude-code".to_string(), "sess-1".to_string());
        assert_eq!(resume_args_for("claude-code", &sessions), vec!["--resume".to_string(), "sess-1".to_string()]);
    }
```

Also add one loop-level round-trip test through `build_sdd_loop_step`. This needs `mock_send` (around lines 1585-1604) to expose the `StepInvocation.args` it was called with, not just `(agent, prompt)` — extend its recorded tuple to `(agent, prompt, args)` and update every existing call site reading `calls.lock().unwrap()` accordingly (a handful of sites in the `sdd_step`-based tests, each just needs its destructuring widened by one field). Then add:

```rust
    #[test]
    fn sdd_loop_resumes_the_implementer_session_on_the_next_fix_round() {
        let (send, calls) = mock_send(&[
            &encode_session("did the thing", Some("sess-1")),
            "REVIEW_ISSUES: needs a test",
            "did the fix",
        ]);
        let step = sdd_step(send);
        let mut ctx = WorkflowContext::new(WorkflowRunId::new(), one_task_data());

        futures::executor::block_on((step.executor.execute)(&mut ctx)).unwrap();
        assert_eq!(ctx.data.agent_sessions.get("implementer"), Some(&"sess-1".to_string()));
        assert_eq!(ctx.data.session_id.as_deref(), Some("sess-1"));

        futures::executor::block_on((step.executor.execute)(&mut ctx)).unwrap();
        futures::executor::block_on((step.executor.execute)(&mut ctx)).unwrap();

        let (agent, _prompt, args) = calls.lock().unwrap().last().unwrap().clone();
        assert_eq!(agent, "implementer");
        assert_eq!(args, vec!["--resume".to_string(), "sess-1".to_string()]);
    }
```

(Reuse whatever implementer `agent_name` string convention this file's existing tests already use, e.g. `sdd_loop_dispatches_implementer_and_review_roles_on_their_own_agents` — don't invent a new fixture.)

- [ ] Step 7: Run: `cargo test -p agentflare work_item_pipeline` — expect all PASS (pre-existing tests unaffected — mock replies with no marker round-trip unchanged).
- [ ] Step 8: Commit: `feat(work-item-pipeline): resume provider sessions across sdd-loop turns`

---

### Task 5: Full verification pass

- [ ] Step 1: Run `cargo test --workspace` — expect PASS. Double-check `src/cli/work.rs`'s `mock_sdd_send` (a second `SendMessage` mock) also round-trips cleanly through `strip_session_marker` (it will, since its replies carry no marker).
- [ ] Step 2: Run `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check` — expect clean.
- [ ] Step 3: Push the branch and open a PR via the normal `item done` flow (summarize what changed and why in the PR body).

Do not attempt a live smoke test against real `cursor-agent`/`claude` binaries as part of this dispatched job — the empirical verification already happened during scoping (see the JSON transcripts above) and the mocked test suite covers the wiring. Just get Tasks 1-5 green and open the PR.
