use clap::{Args, Subcommand};

/// Manage this repo's `.agentflare/config.toml` `[bridge]` overrides, used
/// by CLI/MCP call sites that publish or claim work through the GitHub
/// bridge (e.g. `handoff`'s `recipient="github"` path). Does NOT configure
/// the standalone daemon's claiming loop -- it has no reliable cwd, so it
/// only reads `AGENTFLARE_BRIDGE_ENABLED`/`AGENTFLARE_BRIDGE_REPO` env vars.
#[derive(Args)]
pub struct GithubBridgeArgs {
    #[command(subcommand)]
    pub command: GithubBridgeSubcommand,
}

#[derive(Subcommand)]
pub enum GithubBridgeSubcommand {
    /// Set repo/queue-label overrides for this repo.
    Set {
        /// Repo to publish/claim issues on (owner/repo). Defaults to this repo's origin remote when omitted.
        #[arg(long)]
        repo: Option<String>,
        /// Issue label marking the bridge's pull queue.
        #[arg(long)]
        queue_label: Option<String>,
    },
    /// Remove this repo's overrides, falling back to env vars / origin remote / defaults.
    Unset,
    /// Show the effective repo/queue-label for the current repo.
    Status,
}

impl GithubBridgeArgs {
    pub fn run(self) {
        match self.command {
            GithubBridgeSubcommand::Set { repo, queue_label } => cmd_set(repo, queue_label),
            GithubBridgeSubcommand::Unset => cmd_unset(),
            GithubBridgeSubcommand::Status => cmd_status(),
        }
    }
}

fn repo_root_or_exit() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    match flare_git_core::branch::repo_toplevel(&cwd) {
        Some(root) => root,
        None => {
            crate::ui::error("not inside a git repository");
            std::process::exit(1);
        }
    }
}

fn cmd_set(repo: Option<String>, queue_label: Option<String>) {
    if repo.is_none() && queue_label.is_none() {
        crate::ui::error("pass --repo and/or --queue-label");
        std::process::exit(1);
    }
    // Validate before persisting -- an unparseable `--repo` written as-is
    // would only surface as a confusing failure the next time something
    // resolves it, far from where the typo was made.
    if let Some(r) = &repo
        && crate::github::RepoId::parse(r).is_none()
    {
        crate::ui::error(&format!("--repo {r:?} is not a valid owner/repo"));
        std::process::exit(1);
    }
    let root = repo_root_or_exit();
    match crate::github::bridge::config::write_project_bridge_settings(
        &root,
        repo.as_deref(),
        queue_label.as_deref(),
    ) {
        Ok(path) => crate::ui::success(&format!("wrote {}", path.display())),
        Err(e) => {
            crate::ui::error(&e.to_string());
            std::process::exit(1);
        }
    }
}

fn cmd_unset() {
    let root = repo_root_or_exit();
    match crate::github::bridge::config::clear_project_bridge_settings(&root) {
        Ok(path) => crate::ui::success(&format!("cleared [bridge] overrides in {}", path.display())),
        Err(e) => {
            crate::ui::error(&e.to_string());
            std::process::exit(1);
        }
    }
}

fn cmd_status() {
    let root = repo_root_or_exit();
    let repo = crate::github::bridge::config::resolve_project_repo(&root);
    let queue_label = crate::github::bridge::config::resolve_project_queue_label(&root);
    println!(
        "repo:        {}",
        match repo {
            Ok(Some(r)) => r.to_string(),
            Ok(None) => "(none resolved)".to_string(),
            Err(e) => format!("(error: {e})"),
        }
    );
    println!("queue_label: {queue_label}");
    println!();
    crate::ui::info(
        "this is what CLI/MCP calls (e.g. handoff) resolve from this repo. \
         The background daemon's claiming loop is unaffected -- it only reads \
         AGENTFLARE_BRIDGE_ENABLED/_REPO env vars, since it has no cwd to resolve this file from.",
    );
}
