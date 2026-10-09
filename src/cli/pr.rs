use clap::{Args, Subcommand};

/// Open PRs with an origin stamp and manage cross-instance PR ownership
/// (item #347).
#[derive(Args)]
pub struct PrArgs {
    #[command(subcommand)]
    pub action: PrAction,
}

#[derive(Subcommand)]
pub enum PrAction {
    /// Push the item's branch and open its PR with the origin stamp. Use this
    /// instead of `gh pr create`, which leaves the PR unstamped.
    Open {
        /// Item sequence number (default: parsed from a `task/<seq>-...` branch).
        #[arg(long)]
        item: Option<String>,
        /// PR description (default: a stock line).
        #[arg(long)]
        summary: Option<String>,
    },
    /// Show which instance owns a PR and when it was last active.
    Owner { number: u64 },
    /// Take over a PR another instance owns: refused while the owner shows
    /// activity within the TTL, unless --force.
    Adopt {
        number: u64,
        /// Owner-inactivity TTL in hours.
        #[arg(long, default_value_t = 24)]
        ttl_hours: i64,
        #[arg(long)]
        force: bool,
    },
}

fn fail(msg: &str) -> ! {
    crate::ui::error(msg);
    std::process::exit(1);
}

fn github(
    mcp: &crate::mcp_server::AgentflareMcp,
) -> (crate::github::RepoId, crate::github::Client) {
    let root = mcp.worktree_repo_root();
    let repo = crate::github::RepoId::resolve_from_remote(&root)
        .unwrap_or_else(|| fail("origin is not a GitHub remote"));
    let client = crate::github::Client::new().unwrap_or_else(|e| fail(&format!("{e}")));
    (repo, client)
}

fn comments(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    number: u64,
) -> Vec<(u64, String)> {
    crate::github::issues::list_comments(client, repo, number, None)
        .unwrap_or_else(|e| fail(&format!("could not read PR #{number} comments: {e}")))
        .into_iter()
        .map(|c| (c.id, c.body))
        .collect()
}

/// `task/<seq>-slug` -> `<seq>`.
fn seq_from_branch(branch: &str) -> Option<&str> {
    let rest = branch.strip_prefix("task/")?;
    let seq = rest.split('-').next()?;
    (!seq.is_empty() && seq.bytes().all(|b| b.is_ascii_digit())).then_some(seq)
}

fn open(item: Option<String>, summary: Option<String>) {
    let mcp = crate::mcp_server::AgentflareMcp::default();
    let seq = item.unwrap_or_else(|| {
        let root = crate::mcp_server::AgentflareMcp::repo_root();
        let branch = flare_git_core::shell::run_in_opt(&root, &["branch", "--show-current"])
            .unwrap_or_default();
        seq_from_branch(branch.trim())
            .map(str::to_string)
            .unwrap_or_else(|| fail("not on a task/<seq>-... branch; pass --item <seq>"))
    });
    let repo_root = mcp.worktree_repo_root();
    let resolved = mcp
        .with_backend_db(|conn| {
            let id = mcp.resolve_item_id(conn, &seq)?;
            let item = agentflare_backend::item::get(conn, &id)
                .map_err(crate::mcp_server::types::map_backend_err)?;
            let target = crate::worktree::resolve_target_branch(conn, &item, &repo_root);
            Ok::<_, rmcp::ErrorData>((item, target))
        })
        .unwrap_or_else(|e| fail(&e.to_string()))
        .unwrap_or_else(|e| fail(&e.to_string()));
    let (item, target) = resolved;
    let agent = flare_process::agent_name().unwrap_or_else(|| "agentflare".into());
    match crate::worktree::push_and_open_pr(
        &item,
        &agent,
        &repo_root,
        &target,
        None,
        summary.as_deref(),
    ) {
        crate::worktree::PrOutcome::Opened(pr) => println!("{}", pr.url),
        crate::worktree::PrOutcome::NothingToPush => {
            fail("nothing to push: the branch has no new commits")
        }
        crate::worktree::PrOutcome::NoPrPossible { reason, .. } => fail(&reason),
        crate::worktree::PrOutcome::Failed(reason) => fail(&reason),
    }
}

fn owner(number: u64) {
    let mcp = crate::mcp_server::AgentflareMcp::default();
    let (repo, client) = github(&mcp);
    let pr = crate::github::pulls::get(&client, &repo, number)
        .unwrap_or_else(|e| fail(&format!("could not read PR #{number}: {e}")));
    let found = crate::github::pr_owner::resolve_owner(
        pr.body.as_deref(),
        &comments(&client, &repo, number),
    );
    let me = crate::github::bridge::config::stable_instance_id();
    match found {
        Some(o) => println!(
            "PR #{number}: owner {} ({:?}){} -- last activity {}",
            o.instance,
            o.source,
            if o.instance == me {
                ", this instance"
            } else {
                ""
            },
            pr.updated_at.as_deref().unwrap_or("unknown"),
        ),
        None => println!("PR #{number}: no owner recorded"),
    }
}

fn adopt(number: u64, ttl_hours: i64, force: bool) {
    let mcp = crate::mcp_server::AgentflareMcp::default();
    let (repo, client) = github(&mcp);
    let pr = crate::github::pulls::get(&client, &repo, number)
        .unwrap_or_else(|e| fail(&format!("could not read PR #{number}: {e}")));
    let me = crate::github::bridge::config::stable_instance_id();
    let current = crate::github::pr_owner::resolve_owner(
        pr.body.as_deref(),
        &comments(&client, &repo, number),
    );
    if let Err(why) = crate::github::pr_owner::adopt_check(
        current.as_ref(),
        &me,
        pr.updated_at.as_deref(),
        chrono::Utc::now(),
        ttl_hours.saturating_mul(3600),
        force,
    ) {
        fail(&why);
    }
    crate::github::issues::comment(
        &client,
        &repo,
        number,
        &crate::github::pr_owner::takeover_comment(&me, number),
    )
    .unwrap_or_else(|e| fail(&format!("could not post takeover marker: {e}")));
    // The gates read the body stamp, not the takeover comment: re-point it.
    if let Some(body) = pr
        .body
        .as_deref()
        .and_then(|b| crate::github::pulls::restamp_origin(b, &me))
        && let Err(e) = crate::github::pulls::update_body(&client, &repo, number, &body)
    {
        eprintln!("warning: could not re-stamp PR #{number}: {e}");
    }
    // Swap the beacon so exactly one instance's label remains.
    for l in pr.labels.iter().filter(|l| l.name.starts_with("beacon:")) {
        let _ = crate::github::issues::remove_label(&client, &repo, number, &l.name);
    }
    let beacon = format!("beacon:{}", crate::github::bridge::config::machine_label());
    if let Err(e) = crate::github::issues::add_labels(&client, &repo, number, &[beacon]) {
        eprintln!("warning: could not label PR #{number}: {e}");
    }
    let branch = pr
        .head
        .as_ref()
        .map(|h| h.git_ref.clone())
        .unwrap_or_default();
    let tracked = mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn)?;
        let items = agentflare_backend::item::list_by_project(conn, &project.id)
            .map_err(crate::mcp_server::types::map_backend_err)?;
        if crate::worktree::tracked_pr_numbers(&items).contains(&number) {
            return Ok(false);
        }
        let states = agentflare_backend::state::list_by_project(conn, &project.id)
            .map_err(crate::mcp_server::types::map_backend_err)?;
        let Some(state) = states.iter().find(|s| s.group_name == "in_review") else {
            return Ok(false);
        };
        crate::worktree::create_tracking_item(conn, &project.id, &state.id, &pr, &branch)
            .map_err(crate::mcp_server::types::map_backend_err)?;
        Ok::<_, rmcp::ErrorData>(true)
    });
    match tracked {
        Ok(Ok(true)) => println!("adopted PR #{number}; now tracked locally"),
        Ok(Ok(false)) => {
            println!("adopted PR #{number} (already tracked locally or no in_review state)")
        }
        Ok(Err(e)) => eprintln!("adopted PR #{number} but could not track it locally: {e}"),
        Err(e) => eprintln!("adopted PR #{number} but could not track it locally: {e}"),
    }
}

impl PrArgs {
    pub fn run(self) {
        match self.action {
            PrAction::Open { item, summary } => open(item, summary),
            PrAction::Owner { number } => owner(number),
            PrAction::Adopt {
                number,
                ttl_hours,
                force,
            } => adopt(number, ttl_hours, force),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::seq_from_branch;

    #[test]
    fn seq_from_branch_parses_task_branches_only() {
        assert_eq!(seq_from_branch("task/348-cross-instance"), Some("348"));
        assert_eq!(seq_from_branch("task/348"), Some("348"));
        assert_eq!(seq_from_branch("feature/348-x"), None);
        assert_eq!(seq_from_branch("task/abc"), None);
    }
}
