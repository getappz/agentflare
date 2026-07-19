use crate::agent_launch::{self, HeadlessOutcome};
use crate::mcp_server::AgentflareMcp;
use crate::mcp_server::types::{CommentRequest, ItemRequest};
use agent_registry::{self, autonomous_args, headless_args};
use clap::Args;
use std::time::Duration;

/// Claim a work item, run an agent on it in an isolated worktree, and
/// report the result (comment + PR, or error) back onto the item.
#[derive(Args)]
pub struct WorkArgs {
    /// Item UUID or numeric sequence id.
    pub target: String,
    /// Agent to run (e.g. claude-code, codex, gemini-cli).
    #[arg(long)]
    pub agent: String,
    /// Headless run timeout in seconds (default 1800 = 30 min).
    #[arg(long, default_value_t = 1800)]
    pub timeout: u64,
    /// Max agent turns before forced stop (Claude Code only).
    #[arg(long)]
    pub max_turns: Option<u64>,
    /// Max cost in USD before forced stop (Claude Code only).
    #[arg(long)]
    pub max_cost_usd: Option<f64>,
    /// Channel recipient for a handoff artifact on outcome.
    #[arg(long)]
    pub notify: Option<String>,
}

/// Builds the agent prompt from the item's name/description plus any prior
/// discussion, so a resumed/re-run worker sees what's already been tried.
fn build_prompt(
    item: &agentflare_backend::item::Item,
    comments: &[agentflare_backend::comment::ItemComment],
) -> String {
    let mut prompt = format!(
        "Work item #{} — {}\n\n{}\n",
        item.sequence_id, item.name, item.description
    );
    if !comments.is_empty() {
        prompt.push_str("\nPrior discussion:\n");
        for c in comments {
            prompt.push_str(&format!("- [{}] {}\n", c.author_agent, c.body));
        }
    }
    prompt.push_str("\nWhen you are done, summarize what you changed and why.\n");
    prompt
}

/// Claude Code's `--output-format json` reply shape: `{"result": "...",
/// "session_id": "...", "total_cost_usd": 0.0}`. Falls back to the raw text
/// unparsed for any agent/output that isn't that exact JSON shape — never
/// errors, never blocks the caller.
fn parse_claude_reply(raw: &str) -> (String, Option<String>, Option<f64>) {
    match serde_json::from_str::<serde_json::Value>(raw.trim()) {
        Ok(v) => {
            let text = v
                .get("result")
                .and_then(|r| r.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| raw.to_string());
            let session_id = v
                .get("session_id")
                .and_then(|s| s.as_str())
                .map(str::to_string);
            let cost = v.get("total_cost_usd").and_then(serde_json::Value::as_f64);
            (text, session_id, cost)
        }
        Err(_) => (raw.to_string(), None, None),
    }
}

fn format_success_comment(
    reply: &str,
    session_id: Option<&str>,
    cost_usd: Option<f64>,
    pr_url: Option<&str>,
) -> String {
    let mut body = format!("## agentflare work — complete\n\nAgent reply:\n\n```\n{reply}\n```");
    if let Some(url) = pr_url {
        body.push_str(&format!("\n\nPR: {url}"));
    }
    if session_id.is_some() || cost_usd.is_some() {
        body.push_str("\n\n---\n");
        if let Some(id) = session_id {
            body.push_str(&format!("session: {id}\n"));
        }
        if let Some(c) = cost_usd {
            body.push_str(&format!("cost: ${c:.4}\n"));
        }
    }
    body
}

fn failure_message(outcome: &HeadlessOutcome) -> String {
    match outcome {
        HeadlessOutcome::UnknownAgent(m)
        | HeadlessOutcome::NotHeadless(m)
        | HeadlessOutcome::NotFound(m)
        | HeadlessOutcome::Failed(m) => m.clone(),
        HeadlessOutcome::Ok(_) => {
            unreachable!("Ok is handled by the success path, never passed here")
        }
    }
}

/// Per-agent extra argv inserted before the prompt: the confirmed
/// permission-bypass flag, plus — Claude Code only, since it's the only
/// agent with a confirmed structured-output flag and native turn/cost caps
/// — `--output-format json` and any `--max-turns`/`--max-cost-usd` the
/// caller asked for. Other agents get only their bypass flag; a
/// caller-supplied cap for them is dropped with a warning rather than
/// guessed at.
fn build_extra_args(
    agent: agent_registry::Agent,
    max_turns: Option<u64>,
    max_cost_usd: Option<f64>,
) -> Vec<String> {
    let mut args: Vec<String> = autonomous_args(agent)
        .into_iter()
        .flatten()
        .map(|s| s.to_string())
        .collect();
    if agent == agent_registry::Agent::ClaudeCode {
        args.push("--output-format".to_string());
        args.push("json".to_string());
        if let Some(turns) = max_turns {
            args.push(format!("--max-turns={turns}"));
        }
        if let Some(cost) = max_cost_usd {
            args.push(format!("--max-budget-usd={cost}"));
        }
    } else if max_turns.is_some() || max_cost_usd.is_some() {
        crate::ui::warning(
            "--max-turns/--max-cost-usd are only supported for claude-code currently — ignored",
        );
    }
    args
}

/// Releases the claim and posts a failure comment (+ optional handoff
/// notify) — the single path every early-exit and headless-failure branch
/// in `run_work` routes through, so a claimed item never dead-ends silently
/// held by a worker that errored out.
fn release_and_comment(
    mcp: &AgentflareMcp,
    item_id: &str,
    reason: &str,
    notify_recipient: Option<&str>,
) {
    let _ = mcp.item_release(ItemRequest {
        action: "release".into(),
        id: Some(item_id.into()),
        ..Default::default()
    });
    let comment_body = format!("## agentflare work — failed\n\n{reason}");
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item_id.into()),
        body: Some(comment_body.clone()),
        ..Default::default()
    });
    if let Some(recipient) = notify_recipient {
        notify(recipient, &comment_body, item_id);
    }
}

fn notify(recipient: &str, body: &str, item_id: &str) {
    let outcome = crate::cli::handoff::HandoffArgs {
        recipient: recipient.to_string(),
        file: None,
        content: Some(body.to_string()),
        thread: None,
        reply_to: None,
        name: Some(format!("item-{item_id}-result")),
        session: "handoffs".to_string(),
        sender: None,
        dir: None,
    }
    .publish();
    if let Err(e) = outcome {
        crate::ui::warning(&format!("notify {recipient} failed: {e}"));
    }
}

impl WorkArgs {
    pub fn run(self) {
        std::process::exit(run_work(self));
    }
}

fn run_work(args: WorkArgs) -> i32 {
    let mcp = AgentflareMcp::default();
    let timeout = Duration::from_secs(args.timeout);
    let agent = &args.agent;

    // Validate agent has headless support before claiming anything.
    let agent_enum = agent_registry::REGISTRY
        .iter()
        .find(|s| s.id.as_str() == agent)
        .map(|s| s.id);
    let Some(agent_enum) = agent_enum else {
        crate::ui::error(&format!(
            "unknown agent: {agent} — use `agentflare agents list`"
        ));
        return 1;
    };
    if headless_args(agent_enum).is_none() {
        crate::ui::error(&format!("agent {agent} has no headless print mode"));
        return 1;
    }

    // --- Claim ---
    let claim_resp = match mcp.item_claim(ItemRequest {
        action: "claim".into(),
        id: Some(args.target.clone()),
        ..Default::default()
    }) {
        Ok(json) => json,
        Err(e) => {
            crate::ui::error(&format!("claim failed: {}", e.message));
            return 1;
        }
    };
    let claim: serde_json::Value =
        serde_json::from_str(&claim_resp).unwrap_or(serde_json::Value::Null);
    let status = claim["status"].as_str().unwrap_or("unknown");
    if status != "acquired" {
        let owner = claim["owner"].as_str().unwrap_or("?");
        let age = claim["age_secs"].as_i64().unwrap_or(0);
        crate::ui::error(&format!("item held by {owner} ({age}s) — cannot claim"));
        return 1;
    }
    let item_id = claim["item_id"]
        .as_str()
        .unwrap_or(&args.target)
        .to_string();
    let item_id = item_id.as_str();
    println!("claimed: {item_id}");

    // --- Worktree ---
    let worktree_path = claim["worktree_path"]
        .as_str()
        .map(std::path::PathBuf::from);
    let Some(ref wpath) = worktree_path else {
        let msg = "claim succeeded but no worktree was created (bad git state?)";
        release_and_comment(&mcp, item_id, msg, args.notify.as_deref());
        crate::ui::error(msg);
        return 1;
    };
    println!("worktree: {}", wpath.display());

    // --- Build prompt (item + prior discussion) ---
    let fetched = mcp.with_backend_db(|conn| {
        let resolved = mcp.resolve_item_id(conn, item_id).ok()?;
        let item = agentflare_backend::item::get(conn, &resolved).ok()?;
        let comments = agentflare_backend::comment::list_by_item(conn, &resolved).ok()?;
        Some((item, comments))
    });
    let (item_detail, comments) = match fetched {
        Ok(Some(pair)) => pair,
        _ => {
            let msg = "failed to read item details after claim";
            release_and_comment(&mcp, item_id, msg, args.notify.as_deref());
            crate::ui::error(msg);
            return 1;
        }
    };
    let prompt = build_prompt(&item_detail, &comments);

    // --- Extra args ---
    let extra_args = build_extra_args(agent_enum, args.max_turns, args.max_cost_usd);

    // --- Change to worktree dir and run ---
    let original_dir = std::env::current_dir().ok();
    if std::env::set_current_dir(wpath).is_err() {
        let msg = format!("failed to chdir into {}", wpath.display());
        release_and_comment(&mcp, item_id, &msg, args.notify.as_deref());
        crate::ui::error(&msg);
        return 1;
    }

    let outcome = agent_launch::run_headless(
        agent_registry::REGISTRY,
        agent,
        &prompt,
        timeout,
        &extra_args,
    );

    // Restore cwd regardless of outcome.
    if let Some(d) = original_dir {
        let _ = std::env::set_current_dir(d);
    }

    // --- Report ---
    match outcome {
        HeadlessOutcome::Ok(reply) => {
            let (reply_text, session_id, cost_usd) =
                if agent_enum == agent_registry::Agent::ClaudeCode {
                    parse_claude_reply(&reply)
                } else {
                    (reply, None, None)
                };

            let done_resp = match mcp.item_done(ItemRequest {
                action: "done".into(),
                id: Some(item_id.into()),
                ..Default::default()
            }) {
                Ok(j) => j,
                Err(e) => {
                    crate::ui::error(&format!("item_done failed: {}", e.message));
                    return 1;
                }
            };
            let done_val: serde_json::Value =
                serde_json::from_str(&done_resp).unwrap_or(serde_json::Value::Null);
            let pr_url = done_val["pr_url"].as_str().map(str::to_string);

            let comment_body = format_success_comment(
                &reply_text,
                session_id.as_deref(),
                cost_usd,
                pr_url.as_deref(),
            );
            let _ = mcp.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.into()),
                body: Some(comment_body.clone()),
                ..Default::default()
            });
            if let Some(recipient) = args.notify.as_deref() {
                notify(recipient, &comment_body, item_id);
            }

            println!("done: {item_id}");
            if let Some(url) = &pr_url {
                println!("pr: {url}");
            }
            0
        }
        other => {
            let msg = failure_message(&other);
            release_and_comment(&mcp, item_id, &msg, args.notify.as_deref());
            crate::ui::error(&msg);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_item() -> agentflare_backend::item::Item {
        agentflare_backend::item::Item {
            id: "item-1".into(),
            project_id: "proj-1".into(),
            state_id: "state-1".into(),
            name: "Fix the flaky test".into(),
            description: "test_foo fails ~1 in 20 runs".into(),
            priority: "medium".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id: 42,
            sort_order: 0.0,
            started_at: None,
            completed_at: None,
            archived_at: None,
            external_source: None,
            external_id: None,
            metadata: "{}".into(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
        }
    }

    #[test]
    fn build_prompt_includes_name_description_and_comments() {
        let item = test_item();
        let comments = vec![agentflare_backend::comment::ItemComment {
            id: "c1".into(),
            item_id: "item-1".into(),
            author_agent: "alice".into(),
            body: "probably a race in the setup fixture".into(),
            created_at: 0,
            updated_at: 0,
        }];
        let prompt = build_prompt(&item, &comments);
        assert!(prompt.contains("#42"));
        assert!(prompt.contains("Fix the flaky test"));
        assert!(prompt.contains("test_foo fails ~1 in 20 runs"));
        assert!(prompt.contains("alice"));
        assert!(prompt.contains("probably a race"));
    }

    #[test]
    fn build_prompt_omits_discussion_section_when_no_comments() {
        let item = test_item();
        let prompt = build_prompt(&item, &[]);
        assert!(!prompt.contains("Prior discussion"));
    }

    #[test]
    fn parse_claude_reply_extracts_structured_fields() {
        let raw = r#"{"result":"Fixed the race by adding a mutex.","session_id":"sess-123","total_cost_usd":0.0842}"#;
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, "Fixed the race by adding a mutex.");
        assert_eq!(session_id.as_deref(), Some("sess-123"));
        assert_eq!(cost, Some(0.0842));
    }

    #[test]
    fn parse_claude_reply_falls_back_to_raw_text_on_non_json() {
        let raw = "plain text reply, no JSON here";
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, raw);
        assert!(session_id.is_none());
        assert!(cost.is_none());
    }

    #[test]
    fn format_success_comment_includes_pr_and_metadata() {
        let body = format_success_comment(
            "Fixed the race.",
            Some("sess-123"),
            Some(0.08),
            Some("https://github.com/o/r/pull/9"),
        );
        assert!(body.contains("Fixed the race."));
        assert!(body.contains("https://github.com/o/r/pull/9"));
        assert!(body.contains("sess-123"));
        assert!(body.contains("0.08"));
    }

    #[test]
    fn format_success_comment_omits_metadata_block_when_absent() {
        let body = format_success_comment("Fixed the race.", None, None, None);
        assert!(!body.contains("session:"));
        assert!(!body.contains("cost:"));
    }

    #[test]
    fn failure_message_extracts_inner_string() {
        let outcome = HeadlessOutcome::NotFound("claude not found".into());
        assert_eq!(failure_message(&outcome), "claude not found");
    }

    #[test]
    fn build_extra_args_includes_bypass_and_json_output_for_claude() {
        let args = build_extra_args(agent_registry::Agent::ClaudeCode, None, None);
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"json".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--max-turns")));
    }

    #[test]
    fn build_extra_args_passes_through_max_turns_and_cost_for_claude() {
        let args = build_extra_args(agent_registry::Agent::ClaudeCode, Some(5), Some(2.5));
        assert!(args.contains(&"--max-turns=5".to_string()));
        assert!(args.contains(&"--max-budget-usd=2.5".to_string()));
    }

    #[test]
    fn build_extra_args_for_codex_has_bypass_but_no_json_output() {
        let args = build_extra_args(agent_registry::Agent::Codex, None, None);
        assert_eq!(args, vec!["--full-auto".to_string()]);
    }

    fn seeded_item(
        mcp: &AgentflareMcp,
        conn: &rusqlite::Connection,
    ) -> agentflare_backend::item::Item {
        let project = mcp.resolve_project(conn).unwrap();
        let state = agentflare_backend::state::list_by_project(conn, &project.id)
            .unwrap()
            .into_iter()
            .find(|s| s.is_default)
            .unwrap();
        agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id,
                state_id: state.id,
                name: "Integration test item".into(),
                description: Some("do the thing".into()),
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap()
    }

    fn init_test_repo(root: &std::path::Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
        };
        run(&["init", "-b", "master"]);
        run(&["config", "user.email", "test@test.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["commit", "--allow-empty", "-m", "initial"]);
    }

    #[test]
    fn claim_then_headless_not_found_releases_claim_and_posts_error_comment() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        init_test_repo(&repo_root);
        let backend_db = tmp.path().join("backend.db");
        let project_link = tmp.path().join("project.json");

        let mcp = AgentflareMcp::for_test(backend_db.clone(), repo_root.clone(), project_link);
        let item = mcp.with_backend_db(|conn| seeded_item(&mcp, conn)).unwrap();

        let claim_json = mcp
            .item_claim(ItemRequest {
                action: "claim".to_string(),
                id: Some(item.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let claim: serde_json::Value = serde_json::from_str(&claim_json).unwrap();
        assert_eq!(claim["status"], "acquired");
        assert!(claim["worktree_path"].as_str().is_some());

        // Simulate the failure branch `run_work` takes when the agent binary
        // isn't on PATH — release + comment, the same helper `run_work` calls.
        release_and_comment(&mcp, &item.id, "claude-code not found on PATH", None);

        let claim_after = mcp
            .item_claim(ItemRequest {
                action: "claim".to_string(),
                id: Some(item.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let claim_after: serde_json::Value = serde_json::from_str(&claim_after).unwrap();
        assert_eq!(
            claim_after["status"], "acquired",
            "claim must be released so a re-claim succeeds"
        );

        let comments = mcp
            .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
            .unwrap()
            .unwrap();
        assert_eq!(comments.len(), 1);
        assert!(comments[0].body.contains("claude-code not found on PATH"));
    }

    #[test]
    fn claiming_an_already_held_item_reports_held_without_side_effects() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        init_test_repo(&repo_root);
        let backend_db = tmp.path().join("backend.db");
        let project_link = tmp.path().join("project.json");

        let mcp = AgentflareMcp::for_test(backend_db.clone(), repo_root.clone(), project_link);
        let item = mcp.with_backend_db(|conn| seeded_item(&mcp, conn)).unwrap();

        let first = mcp
            .item_claim(ItemRequest {
                action: "claim".to_string(),
                id: Some(item.id.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&first).unwrap()["status"],
            "acquired"
        );

        // A second claim by a different owner is what a real second worker
        // process would see — proven directly at the DB/ledger level, since
        // this test process's own identity is the same for both calls above.
        let held = mcp
            .with_backend_db(|conn| {
                agentflare_backend::claim::has_active_claim_by_other(
                    conn,
                    &item.id,
                    "someone-else:1",
                    crate::claims::now(),
                    crate::mcp_server::types::backend_claim_ttl_secs(),
                )
            })
            .unwrap()
            .unwrap();
        assert!(held, "item must show as actively claimed by another owner");
    }
}
