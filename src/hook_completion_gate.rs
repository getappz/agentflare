// PostToolUse (success) command hook for item #169's combined completion
// gate. Split out of hook.rs (which was over the repo's LOC gate) rather
// than left inline -- see hook.rs's module doc comment for the general
// "no install/consent logic here, just runtime reinforcement" convention
// this follows too.
use crate::hook::read_stdin_or_skip;
use serde_json::{Value, json};

struct PostToolUseInput {
    session_id: String,
    tool_name: String,
    command: Option<String>,
    exit_code: Option<i32>,
    output_text: String,
    item_action: Option<String>,
    /// Whether the `item` call's own response says the action actually took
    /// effect (`done: true` for action=done, `promoted: true` for
    /// action=check_merge) -- `None` when there was no `item_action`, or the
    /// response shape couldn't be read at all (unparseable/missing), so
    /// callers should treat `None` as "unknown", not "failed".
    item_success: Option<bool>,
}

/// Best-effort parse of a `tool_response` value into a JSON object,
/// covering the shapes it's plausibly sent in: already a parsed object; a
/// JSON-encoded string (`item`'s own handlers return `resp.to_string()`);
/// or Claude Code's MCP content-block envelope
/// (`{"content":[{"type":"text","text":"<json>"}]}`). `None` if none of
/// these parse -- same "unknown, not false" spirit as the rest of this
/// module's defensive field lookups.
fn response_as_json(response: &Value) -> Option<Value> {
    if response.is_object() && response.get("content").is_none() {
        return Some(response.clone());
    }
    if let Some(s) = response.as_str() {
        return serde_json::from_str(s).ok();
    }
    response
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str(text).ok())
}

/// Reads whether an `item done`/`check_merge` call's response says the
/// action actually took effect -- see [`PostToolUseInput::item_success`].
fn item_action_succeeded(action: &str, response: Option<&Value>) -> Option<bool> {
    let parsed = response_as_json(response?)?;
    match action {
        "done" => parsed.get("done").and_then(Value::as_bool),
        "check_merge" => parsed.get("promoted").and_then(Value::as_bool),
        _ => None,
    }
}

/// Extracts what the completion-gate hooks need from a successful
/// PostToolUse stdin payload: for a Bash-family call, the command text and
/// whatever pass/fail signal `tool_response` carries; for an `item` MCP
/// call, which `action` it invoked and whether that action's own response
/// says it actually took effect. The exact shape of `tool_response` for a
/// Bash tool call isn't pinned down anywhere else in this codebase yet
/// (unlike `hook::parse_post_tool_failure`'s "error" field, which was
/// live-verified) -- several plausible field names are tried defensively,
/// same convention as that function, with a text-scan fallback in
/// `verification_passed` below when none of them are present. "status" is
/// deliberately NOT one of the exit-code field names tried: it's common
/// enough as an unrelated field on non-Bash tool responses (e.g. an HTTP
/// status code) that trusting it risked misreading an unrelated field as a
/// process exit code.
fn parse_post_tool_use(input: &str) -> Option<PostToolUseInput> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    let tool_name = v.get("tool_name")?.as_str()?.to_string();
    let tool_input = v.get("tool_input");
    let command = tool_input
        .and_then(|ti| {
            ti.get("command")
                .or_else(|| ti.get("cmd"))
                .or_else(|| ti.get("script"))
        })
        .and_then(Value::as_str)
        .map(String::from);
    let item_action = tool_input
        .and_then(|ti| ti.get("action"))
        .and_then(Value::as_str)
        .map(String::from);
    let response = v.get("tool_response");
    let exit_code = response
        .and_then(|r| {
            ["exit_code", "exitCode", "returncode", "return_code"]
                .iter()
                .find_map(|key| r.get(key))
        })
        .and_then(Value::as_i64)
        .map(|n| n as i32);
    let output_text = response
        .map(|r| {
            let stdout = r.get("stdout").and_then(Value::as_str).unwrap_or("");
            let stderr = r.get("stderr").and_then(Value::as_str).unwrap_or("");
            if stdout.is_empty() && stderr.is_empty() {
                r.as_str().unwrap_or_default().to_string()
            } else {
                format!("{stdout}\n{stderr}")
            }
        })
        .unwrap_or_default();
    let item_success = item_action
        .as_deref()
        .and_then(|action| item_action_succeeded(action, response));
    Some(PostToolUseInput {
        session_id,
        tool_name,
        command,
        exit_code,
        output_text,
        item_action,
        item_success,
    })
}

const VERIFICATION_FAILURE_MARKERS: &[&str] = &[
    "test failed",
    "tests failed",
    "failures:",
    "error[e",
    "panicked at",
    "fatal:",
    "build failed",
    "compilation failed",
    "exit code: 1",
    "exit status 1",
    "non-zero exit",
];

/// Whether a verification command's outcome counts as passing. Prefers an
/// explicit exit code (0 = pass); falls back to scanning combined stdout+
/// stderr for common failure markers when no exit code field was found,
/// defaulting to "passed" when neither signals failure -- the completion
/// gate's primary defense is requiring *some* fresh evidence to exist at
/// all (`hook_redirect::completion_gate_reason`), so an ambiguous pass/fail
/// read errs permissive here rather than blocking on a parsing gap.
fn verification_passed(exit_code: Option<i32>, output_text: &str) -> bool {
    if let Some(code) = exit_code {
        return code == 0;
    }
    let lower = output_text.to_lowercase();
    !VERIFICATION_FAILURE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Agentflare-flavored port of superpowers' `finishing-a-development-branch`
/// decision menu (wording adapted, not copied verbatim -- see
/// `.refs/superpowers/skills/finishing-a-development-branch/SKILL.md`).
/// Surfaced once `item done`/`check_merge` succeeds, so the merge/PR/keep
/// decision is asked explicitly instead of staying implicit in whichever of
/// `branch_guard_reason_for`, `worktree::cleanup_worktree`, or the PM item
/// lifecycle happens to run next.
fn finishing_branch_menu(action: &str) -> String {
    format!(
        "item {action} succeeded. Decide how to integrate this branch -- 1) merge it into the target branch locally and clean up the worktree, 2) push it and open/keep a Pull Request for review (already the default unless `push=false` was passed), or 3) keep the branch and worktree as-is for now. Don't assume; ask if it's not already clear which one applies here."
    )
}

/// Whether this successful tool call is the completion moment the
/// finishing-a-development-branch menu should fire for -- an `item` call
/// (by either name variant, see [`crate::hook_redirect::ITEM_TOOL_NAME`])
/// whose action actually reached `done`/`check_merge` AND whose own
/// response says it took effect (`item_success`) -- a no-op `done` call
/// (item already done) or a `check_merge` that finds the PR not yet merged
/// must not surface "decide how to integrate this branch" for a branch that
/// isn't actually at that decision point yet. `item_success == None` (the
/// response shape couldn't be read) still shows the menu -- better an
/// occasional premature nudge than silently dropping the menu forever if
/// the response-parsing guess in `response_as_json` turns out wrong.
/// Extracted from [`post_tool_use`] so the trigger condition is
/// unit-testable without going through stdin.
fn shows_finishing_branch_menu(tool_name: &str, action: &str, item_success: Option<bool>) -> bool {
    matches!(action, "done" | "check_merge")
        && (tool_name == crate::hook_redirect::ITEM_TOOL_NAME || tool_name == "item")
        && item_success != Some(false)
}

/// PostToolUse (success) command hook. Three independent jobs, all closing
/// gaps from item #169's completion gate: (1) records verification evidence
/// for the session when a Bash-family call's command matches
/// `optimize::is_verification_command` -- this is the ONLY place that
/// evidence is ever recorded, so `hook_redirect::completion_gate_reason`
/// has something to check; (2) surfaces the finishing-a-development-branch
/// decision menu once `item done`/`check_merge` actually succeeds; (3)
/// invalidates any recorded verification evidence when a mutating tool
/// (Write/Edit/MultiEdit/patch/ctx_patch/...) runs, so a test run that
/// passed before this edit can't cover a since-changed tree.
pub fn post_tool_use(_agent: &str) {
    let Some(input) = read_stdin_or_skip("PostToolUse") else {
        return;
    };
    let Some(parsed) = parse_post_tool_use(&input) else {
        return;
    };

    if let Some(action) = &parsed.item_action
        && shows_finishing_branch_menu(&parsed.tool_name, action, parsed.item_success)
    {
        let out = json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": finishing_branch_menu(action),
            }
        });
        println!("{out}");
        return;
    }

    if crate::hook_redirect::MUTATING_TOOLS.contains(&parsed.tool_name.as_str()) {
        let mut runtime = crate::optimize::load_runtime();
        crate::optimize::invalidate_verification(&mut runtime, &parsed.session_id);
        crate::optimize::save_runtime(&runtime);
        return;
    }

    let Some(command) = &parsed.command else {
        return;
    };
    if !crate::optimize::is_verification_command(command) {
        return;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let passed = verification_passed(parsed.exit_code, &parsed.output_text);

    let mut runtime = crate::optimize::load_runtime();
    crate::optimize::prune_stale_sessions(&mut runtime, now);
    let record = runtime
        .sessions
        .entry(parsed.session_id.clone())
        .or_insert_with(|| crate::optimize::SessionRecord {
            start_ts: now,
            turn_count: 0,
            recent_tool_calls: vec![],
            last_verification: None,
        });
    record.last_verification = Some(crate::optimize::VerificationEvidence {
        command: command.clone(),
        exit_code: parsed.exit_code,
        passed,
        ts: now,
    });
    crate::optimize::save_runtime(&runtime);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_post_tool_use_reads_bash_command_and_exit_code() {
        let input = r#"{"session_id":"s1","tool_name":"Bash","tool_input":{"command":"cargo test"},"tool_response":{"exit_code":0,"stdout":"ok","stderr":""}}"#;
        let parsed = parse_post_tool_use(input).unwrap();
        assert_eq!(parsed.session_id, "s1");
        assert_eq!(parsed.command.as_deref(), Some("cargo test"));
        assert_eq!(parsed.exit_code, Some(0));
        assert!(parsed.item_action.is_none());
    }

    #[test]
    fn parse_post_tool_use_reads_item_action() {
        let input = r#"{"session_id":"s1","tool_name":"mcp__flare__item","tool_input":{"action":"done","id":"abc"},"tool_response":{}}"#;
        let parsed = parse_post_tool_use(input).unwrap();
        assert_eq!(parsed.item_action.as_deref(), Some("done"));
        assert!(parsed.command.is_none());
    }

    #[test]
    fn parse_post_tool_use_ignores_status_field_for_exit_code() {
        // Regression: PR #581 review finding #4 -- "status" is generic
        // enough to collide with an unrelated field on some tool shapes
        // (e.g. an HTTP status code), so it must not be read as exit code.
        let input = r#"{"session_id":"s1","tool_name":"Bash","tool_input":{"command":"cargo test"},"tool_response":{"status":200,"stdout":"test result: ok","stderr":""}}"#;
        let parsed = parse_post_tool_use(input).unwrap();
        assert!(parsed.exit_code.is_none());
    }

    #[test]
    fn response_as_json_parses_json_encoded_string_response() {
        let response: Value =
            serde_json::from_str(r#""{\"done\":true,\"item_id\":\"abc\"}""#).unwrap();
        let parsed = response_as_json(&response).unwrap();
        assert_eq!(parsed["done"], true);
    }

    #[test]
    fn response_as_json_parses_mcp_content_block_envelope() {
        let response = json!({
            "content": [{"type": "text", "text": "{\"promoted\":true}"}]
        });
        let parsed = response_as_json(&response).unwrap();
        assert_eq!(parsed["promoted"], true);
    }

    #[test]
    fn response_as_json_passes_through_plain_object() {
        let response = json!({"done": false});
        let parsed = response_as_json(&response).unwrap();
        assert_eq!(parsed["done"], false);
    }

    #[test]
    fn item_action_succeeded_reads_done_flag() {
        let response = json!({"done": true, "status": "in_review"});
        assert_eq!(item_action_succeeded("done", Some(&response)), Some(true));
        let response = json!({"done": false, "status": "unchanged"});
        assert_eq!(item_action_succeeded("done", Some(&response)), Some(false));
    }

    #[test]
    fn item_action_succeeded_reads_promoted_flag() {
        let response = json!({"item_id": "abc", "promoted": false, "reason": "PR not merged yet"});
        assert_eq!(
            item_action_succeeded("check_merge", Some(&response)),
            Some(false)
        );
    }

    #[test]
    fn item_action_succeeded_unknown_when_response_missing() {
        assert_eq!(item_action_succeeded("done", None), None);
    }

    #[test]
    fn parse_post_tool_use_falls_back_to_cmd_and_script_fields() {
        let input = r#"{"session_id":"s1","tool_name":"shell","tool_input":{"cmd":"npm test"}}"#;
        assert_eq!(
            parse_post_tool_use(input).unwrap().command.as_deref(),
            Some("npm test")
        );
        let input = r#"{"session_id":"s1","tool_name":"powershell","tool_input":{"script":"go test ./..."}}"#;
        assert_eq!(
            parse_post_tool_use(input).unwrap().command.as_deref(),
            Some("go test ./...")
        );
    }

    #[test]
    fn parse_post_tool_use_returns_none_on_invalid_json() {
        assert!(parse_post_tool_use("not json").is_none());
    }

    #[test]
    fn verification_passed_trusts_zero_exit_code() {
        assert!(verification_passed(Some(0), ""));
        assert!(!verification_passed(Some(1), "irrelevant text"));
    }

    #[test]
    fn verification_passed_scans_output_when_exit_code_missing() {
        assert!(verification_passed(
            None,
            "running 5 tests\ntest result: ok"
        ));
        assert!(!verification_passed(None, "3 failures:\n  test_foo"));
        assert!(!verification_passed(
            None,
            "thread 'main' panicked at 'boom'"
        ));
    }

    #[test]
    fn finishing_branch_menu_names_the_three_options() {
        let menu = finishing_branch_menu("done");
        assert!(menu.contains("merge"));
        assert!(menu.contains("Pull Request"));
        assert!(menu.contains("keep"));
    }

    #[test]
    fn shows_finishing_branch_menu_triggers_on_done_and_check_merge() {
        assert!(shows_finishing_branch_menu(
            crate::hook_redirect::ITEM_TOOL_NAME,
            "done",
            Some(true)
        ));
        assert!(shows_finishing_branch_menu(
            crate::hook_redirect::ITEM_TOOL_NAME,
            "check_merge",
            Some(true)
        ));
        assert!(shows_finishing_branch_menu("item", "done", Some(true)));
    }

    #[test]
    fn shows_finishing_branch_menu_ignores_other_actions_and_tools() {
        assert!(!shows_finishing_branch_menu(
            crate::hook_redirect::ITEM_TOOL_NAME,
            "claim",
            Some(true)
        ));
        assert!(!shows_finishing_branch_menu(
            crate::hook_redirect::ITEM_TOOL_NAME,
            "update",
            Some(true)
        ));
        assert!(!shows_finishing_branch_menu("Bash", "done", Some(true)));
    }

    #[test]
    fn shows_finishing_branch_menu_suppressed_on_confirmed_no_op() {
        // Regression: PR #581 review finding #3 -- a no-op `done` (item
        // already done) or a `check_merge` whose PR isn't merged yet must
        // not show the "decide how to integrate" menu.
        assert!(!shows_finishing_branch_menu(
            crate::hook_redirect::ITEM_TOOL_NAME,
            "done",
            Some(false)
        ));
        assert!(!shows_finishing_branch_menu(
            crate::hook_redirect::ITEM_TOOL_NAME,
            "check_merge",
            Some(false)
        ));
    }

    #[test]
    fn shows_finishing_branch_menu_shows_when_success_unknown() {
        // Response shape couldn't be read -- err toward showing the menu
        // rather than silently dropping it forever (see doc comment).
        assert!(shows_finishing_branch_menu(
            crate::hook_redirect::ITEM_TOOL_NAME,
            "done",
            None
        ));
    }
}
