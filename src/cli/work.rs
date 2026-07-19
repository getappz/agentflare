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

impl WorkArgs {
    pub fn run(self) {
        let mcp = AgentflareMcp::default();
        let timeout = Duration::from_secs(self.timeout);
        let agent = &self.agent;

        // Validate agent has headless support before claiming anything.
        let agent_enum = agent_registry::REGISTRY
            .iter()
            .find(|s| s.id.as_str() == agent)
            .map(|s| s.id);
        let Some(agent_enum) = agent_enum else {
            crate::ui::error(&format!(
                "unknown agent: {agent} — use `agentflare agents list`"
            ));
            std::process::exit(1);
        };
        if headless_args(agent_enum).is_none() {
            crate::ui::error(&format!("agent {agent} has no headless print mode"));
            std::process::exit(1);
        }

        // --- Claim ---
        let claim_req = ItemRequest {
            action: "claim".into(),
            id: Some(self.target.clone()),
            ..Default::default()
        };
        let claim_resp = match mcp.item_claim(claim_req) {
            Ok(json) => json,
            Err(e) => {
                crate::ui::error(&format!("claim failed: {}", e.message));
                std::process::exit(1);
            }
        };
        let claim: serde_json::Value =
            serde_json::from_str(&claim_resp).unwrap_or(serde_json::Value::Null);
        let status = claim["status"].as_str().unwrap_or("unknown");
        if status != "acquired" {
            let owner = claim["owner"].as_str().unwrap_or("?");
            let age = claim["age_secs"].as_i64().unwrap_or(0);
            crate::ui::error(&format!("item held by {owner} ({age}s) — cannot claim"));
            std::process::exit(1);
        }
        let item_id = claim["item_id"].as_str().unwrap_or(&self.target);
        println!("claimed: {item_id}");

        // --- Worktree ---
        let worktree_path = claim["worktree_path"]
            .as_str()
            .map(std::path::PathBuf::from);
        let Some(ref wpath) = worktree_path else {
            // Claim succeeded without a worktree path — release and exit.
            let _ = mcp.item_release(ItemRequest {
                action: "release".into(),
                id: Some(item_id.into()),
                ..Default::default()
            });
            crate::ui::error("claim succeeded but no worktree was created (bad git state?)");
            std::process::exit(1);
        };
        println!("worktree: {}", wpath.display());

        // --- Build prompt ---
        let item_detail = match mcp.with_backend_db(|conn| {
            let resolved = mcp.resolve_item_id(conn, item_id).ok()?;
            agentflare_backend::item::get(conn, &resolved).ok()
        }) {
            Ok(Some(i)) => i,
            _ => {
                let _ = mcp.item_release(ItemRequest {
                    action: "release".into(),
                    id: Some(item_id.into()),
                    ..Default::default()
                });
                crate::ui::error("failed to read item details after claim");
                std::process::exit(1);
            }
        };
        let prompt = format!("{}\n\n{}", item_detail.name, item_detail.description);

        // --- Build extra args ---
        let mut extra_args: Vec<String> = Vec::new();
        if let Some(autonomous) = autonomous_args(agent_enum) {
            extra_args.extend(autonomous.iter().map(|s| s.to_string()));
        }
        if let Some(turns) = self.max_turns {
            extra_args.push(format!("--max-turns={}", turns));
        }
        if let Some(cost) = self.max_cost_usd {
            extra_args.push(format!("--max-budget-usd={}", cost));
        }

        // --- Change to worktree dir and run ---
        let original_dir = std::env::current_dir().ok();
        if std::env::set_current_dir(wpath).is_err() {
            let _ = mcp.item_release(ItemRequest {
                action: "release".into(),
                id: Some(item_id.into()),
                ..Default::default()
            });
            crate::ui::error(&format!("failed to chdir into {}", wpath.display()));
            std::process::exit(1);
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
        let comment_body;
        let exit_code;
        match outcome {
            HeadlessOutcome::Ok(reply) => {
                // Success: mark done, open PR, post comment.
                let done_req = ItemRequest {
                    action: "done".into(),
                    id: Some(item_id.into()),
                    ..Default::default()
                };
                let done_resp = match mcp.item_done(done_req) {
                    Ok(j) => j,
                    Err(e) => {
                        crate::ui::error(&format!("item_done failed: {}", e.message));
                        std::process::exit(1);
                    }
                };
                let done_val: serde_json::Value =
                    serde_json::from_str(&done_resp).unwrap_or(serde_json::Value::Null);
                let pr_url = done_val["pr_url"].as_str().map(str::to_string);

                let mut report =
                    format!("## agentflare work — complete\n\nAgent reply:\n\n```\n{reply}\n```");
                if let Some(ref url) = pr_url {
                    report.push_str(&format!("\n\nPR: {url}"));
                }
                comment_body = report;

                println!("done: {item_id}");
                if let Some(url) = &pr_url {
                    println!("pr: {url}");
                }
                exit_code = 0;
            }
            HeadlessOutcome::UnknownAgent(msg)
            | HeadlessOutcome::NotHeadless(msg)
            | HeadlessOutcome::NotFound(msg)
            | HeadlessOutcome::Failed(msg) => {
                // Failure: release claim, post error comment.
                let _ = mcp.item_release(ItemRequest {
                    action: "release".into(),
                    id: Some(item_id.into()),
                    ..Default::default()
                });

                comment_body = format!("## agentflare work — failed\n\n{msg}");
                crate::ui::error(&msg);
                exit_code = 1;
            }
        }

        // Post comment on the item.
        let _ = mcp.comment_impl(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.into()),
            body: Some(comment_body),
            ..Default::default()
        });

        std::process::exit(exit_code);
    }
}
