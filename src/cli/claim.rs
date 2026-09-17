use clap::{Args, Subcommand};

/// Claim GitHub issues/PRs so parallel agents don't duplicate work. Backed by
/// the leased work-claim ledger in ~/.agentflare/agentflare.db.
#[derive(Args)]
pub struct ClaimArgs {
    #[command(subcommand)]
    pub action: ClaimAction,
}

#[derive(Subcommand)]
pub enum ClaimAction {
    /// Take ownership of a target (e.g. issue#42). Steals only stale/done claims.
    Acquire {
        /// Target identifier, e.g. "issue#42" or "pr#7".
        target: String,
        /// Repo key (default: normalized origin remote, owner/name).
        #[arg(long)]
        repo: Option<String>,
        /// Path glob(s) this claim owns write scope over (repeatable), e.g.
        /// --scope crates/foo/ --scope docs/foo/. Omit for the back-compat
        /// default (unscoped -- never enforced against other agents).
        #[arg(long)]
        scope: Vec<String>,
    },
    /// Refresh the lease on a target you own.
    Heartbeat {
        target: String,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Release a target you own (frees it for others).
    Release {
        target: String,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Mark a target you own as done (kept for audit; re-acquirable).
    Done {
        target: String,
        #[arg(long)]
        repo: Option<String>,
    },
    /// List claims. By default shows only live claims for the current repo.
    List {
        /// Repo key (default: current repo; ignored with --all-repos).
        #[arg(long)]
        repo: Option<String>,
        /// Include stale and done claims.
        #[arg(long)]
        all: bool,
        /// List across every repo in the ledger.
        #[arg(long)]
        all_repos: bool,
    },
    /// Ask a live claim's owner to stop (human-in-the-loop signal). Honor
    /// system: the owner must poll `should-stop` and act on it -- this does
    /// not itself interrupt or lock anything.
    Stop {
        target: String,
        #[arg(long)]
        repo: Option<String>,
        /// Why the agent should stop -- surfaced back to it by `should-stop`.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Poll whether a stop has been requested for a target. Designed to be
    /// called from inside an agent's own work loop. Exit code: 0 = stop
    /// requested, 1 = continue (no stop signal), >1 = error running the check.
    ShouldStop {
        target: String,
        #[arg(long)]
        repo: Option<String>,
    },
}

impl ClaimArgs {
    pub fn run(self) {
        // `should-stop` reserves exit 1 for its own "continue" outcome, so an
        // infra failure (can't even open the ledger) must exit >1 here or a
        // polling script would misread "ledger unreachable" as "keep going".
        let is_should_stop = matches!(self.action, ClaimAction::ShouldStop { .. });
        let conn = match crate::db::open() {
            Ok(c) => c,
            Err(e) => {
                crate::ui::error(&format!("claim: cannot open ledger: {e}"));
                std::process::exit(if is_should_stop { 2 } else { 1 });
            }
        };
        let owner = crate::claims::owner_id();
        let ttl = crate::claims::ttl_secs();
        let now = crate::claims::now();

        match self.action {
            ClaimAction::Acquire {
                target,
                repo,
                scope,
            } => acquire_cmd(&conn, &owner, ttl, now, target, repo, scope),
            ClaimAction::Heartbeat { target, repo } => {
                let repo = require_repo(repo);
                report(
                    crate::claims::heartbeat(&conn, &repo, &target, &owner, now),
                    "heartbeat",
                    &repo,
                    &target,
                    &owner,
                );
            }
            ClaimAction::Release { target, repo } => {
                let repo = require_repo(repo);
                report(
                    crate::claims::release(&conn, &repo, &target, &owner),
                    "released",
                    &repo,
                    &target,
                    &owner,
                );
            }
            ClaimAction::Done { target, repo } => {
                let repo = require_repo(repo);
                report(
                    crate::claims::done(&conn, &repo, &target, &owner, now),
                    "done",
                    &repo,
                    &target,
                    &owner,
                );
            }
            ClaimAction::List {
                repo,
                all,
                all_repos,
            } => {
                let scope = if all_repos {
                    None
                } else {
                    Some(require_repo(repo))
                };
                match crate::claims::list(&conn, scope.as_deref(), all, now, ttl) {
                    Ok(claims) if claims.is_empty() => println!("no claims"),
                    Ok(claims) => {
                        for c in claims {
                            let flag = if c.status == "done" {
                                " [done]"
                            } else if c.stale {
                                " [stale]"
                            } else {
                                ""
                            };
                            println!("{}  {}  {}{}", c.repo, c.target, c.owner, flag);
                        }
                    }
                    Err(e) => fail(e),
                }
            }
            ClaimAction::Stop {
                target,
                repo,
                reason,
            } => {
                let repo = require_repo(repo);
                match crate::claims::request_stop(&conn, &repo, &target, reason.as_deref(), now) {
                    Ok(true) => println!("stop requested for {repo} {target}"),
                    Ok(false) => {
                        crate::ui::error(&format!("{repo} {target}: no live claim to signal"));
                        std::process::exit(1);
                    }
                    Err(e) => fail(e),
                }
            }
            ClaimAction::ShouldStop { target, repo } => {
                let repo = match crate::claims::resolve_repo(repo) {
                    Some(r) => r,
                    None => {
                        crate::ui::error(
                            "claim: could not determine repo — run inside a git repo or pass --repo owner/name",
                        );
                        std::process::exit(2);
                    }
                };
                match crate::claims::should_stop(&conn, &repo, &target) {
                    Ok(Some(signal)) => {
                        match signal.reason {
                            Some(r) => println!("stop requested: {r}"),
                            None => println!("stop requested"),
                        }
                        std::process::exit(0);
                    }
                    Ok(None) => {
                        println!("continue");
                        std::process::exit(1);
                    }
                    Err(e) => {
                        crate::ui::error(&format!("claim: ledger error: {e}"));
                        std::process::exit(2);
                    }
                }
            }
        }
    }
}

/// `ClaimAction::Acquire` handler, split out to keep `run`'s dispatch match
/// flat now that scope handling adds a warning check on top of the plain
/// acquire/held/error branches.
fn acquire_cmd(
    conn: &rusqlite::Connection,
    owner: &str,
    ttl: i64,
    now: i64,
    target: String,
    repo: Option<String>,
    scope: Vec<String>,
) {
    // Only attach the current checkout's commit when the repo was
    // auto-resolved from it; an explicit --repo may name a different
    // repository, so HEAD here would be misleading provenance.
    let commit = if repo.is_none() { git_commit() } else { None };
    let repo = require_repo(repo);
    let scope_arg = (!scope.is_empty()).then_some(scope.as_slice());
    let clear_warning = crate::claims::scope_clear_warning(conn, &repo, &target, scope_arg)
        .ok()
        .flatten();
    match crate::claims::acquire(
        conn,
        &repo,
        &target,
        owner,
        commit.as_deref(),
        scope_arg,
        now,
        ttl,
    ) {
        Ok(crate::claims::Acquire::Acquired) => {
            println!("claimed {repo} {target}  (owner {owner})");
            if let Some(warning) = clear_warning {
                crate::ui::error(&format!("warning: {warning}"));
            } else if let Some(s) = scope_arg {
                let warning =
                    crate::claims::scope_overlap_warning(conn, &repo, &target, s, now, ttl);
                if let Ok(Some(warning)) = warning {
                    crate::ui::error(&format!("warning: {warning}"));
                }
            }
        }
        Ok(crate::claims::Acquire::Held {
            owner: holder,
            age_secs,
        }) => {
            crate::ui::error(&format!(
                "{repo} {target} already held by {holder} ({age_secs}s since heartbeat)"
            ));
            std::process::exit(1);
        }
        Err(e) => fail(e),
    }
}

/// A verb that returns "did it change my row" → owner-scoped success message.
fn report(res: rusqlite::Result<bool>, verb: &str, repo: &str, target: &str, owner: &str) {
    match res {
        Ok(true) => println!("{verb} {repo} {target}"),
        Ok(false) => {
            crate::ui::error(&format!(
                "{repo} {target} not held by {owner} — nothing changed"
            ));
            std::process::exit(1);
        }
        Err(e) => fail(e),
    }
}

fn require_repo(explicit: Option<String>) -> String {
    crate::claims::resolve_repo(explicit).unwrap_or_else(|| {
        crate::ui::error(
            "claim: could not determine repo — run inside a git repo or pass --repo owner/name",
        );
        std::process::exit(1);
    })
}

fn git_commit() -> Option<String> {
    crate::mcp_server::AgentflareMcp::git_provenance().and_then(|g| g.commit)
}

fn fail(e: rusqlite::Error) -> ! {
    crate::ui::error(&format!("claim: ledger error: {e}"));
    std::process::exit(1);
}
