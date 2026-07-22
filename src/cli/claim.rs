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
}

impl ClaimArgs {
    pub fn run(self) {
        let conn = match crate::db::open() {
            Ok(c) => c,
            Err(e) => {
                crate::ui::error(&format!("claim: cannot open ledger: {e}"));
                std::process::exit(1);
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
            if let Some(s) = scope_arg {
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
