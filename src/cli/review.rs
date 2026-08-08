use clap::{Args, Subcommand};

/// Multi-agent review consensus: finders submit findings, agentflare verifies
/// citations against the diff, dedups, and tags CONFIRMED/UNIQUE/DISPUTED/
/// UNVERIFIED. Stored in ~/.agentflare/agentflare.db.
#[derive(Args)]
pub struct ReviewArgs {
    #[command(subcommand)]
    pub action: ReviewAction,
}

#[derive(Subcommand)]
pub enum ReviewAction {
    /// Submit a finder's findings (JSON array of {file,line,message,severity?,category?})
    /// from --file or stdin. Replaces this agent's prior findings for the round.
    Submit {
        /// Review round id (default: current branch name).
        #[arg(long)]
        pr: Option<String>,
        /// Finder name (default: detected agent).
        #[arg(long)]
        agent: Option<String>,
        /// JSON file of findings (default: read stdin).
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Verify, dedup, and tag all submitted findings into one consensus report.
    Consensus {
        #[arg(long)]
        pr: Option<String>,
        /// Diff base ref (default: master).
        #[arg(long)]
        base: Option<String>,
        /// Diff head ref (default: HEAD).
        #[arg(long)]
        head: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        /// Emit JSON instead of markdown.
        #[arg(long)]
        json: bool,
    },
    /// List the raw submitted findings for a round.
    List {
        #[arg(long)]
        pr: Option<String>,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Drop all submitted findings for a round.
    Clear {
        #[arg(long)]
        pr: Option<String>,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Record this round's per-agent accuracy (verified vs total findings).
    Record {
        #[arg(long)]
        pr: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        head: Option<String>,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Show per-agent accuracy across recorded rounds.
    Scores {
        /// Scope to one repo (default: current repo; ignored with --all-repos).
        #[arg(long)]
        repo: Option<String>,
        /// Aggregate across every repo.
        #[arg(long)]
        all_repos: bool,
        #[arg(long)]
        json: bool,
    },
    /// Compute one agent's project-level performance review (quantity,
    /// quality, cost, attention) and save it to the memory store as a
    /// `performance_review` observation. Run `agentflare memory sync`
    /// afterward to share it with other workstations.
    Performance {
        /// Backend project id (agentflare item-tracker project — run
        /// `agentflare memory observations` or check the dashboard to find
        /// it; this is NOT the same identifier as --repo).
        #[arg(long)]
        project: String,
        /// Agent name (default: detected, same convention as `submit`).
        #[arg(long)]
        agent: Option<String>,
        /// Scope quality scoring to one repo (default: current repo).
        #[arg(long)]
        repo: Option<String>,
        /// Window size in days ending now, for quantity/attention/cost
        /// (quality is always all-time — see `scores`).
        #[arg(long, default_value = "7")]
        days: i64,
        #[arg(long)]
        json: bool,
    },
}

impl ReviewArgs {
    pub fn run(self) {
        let conn = match crate::db::open() {
            Ok(c) => c,
            Err(e) => fail(format!("cannot open ledger: {e}")),
        };
        match self.action {
            ReviewAction::Submit {
                pr,
                agent,
                file,
                repo,
            } => {
                let repo = require_repo(repo);
                let pr = resolve_pr(pr);
                let agent = agent.unwrap_or_else(crate::review::submitter_name);
                let raw = match &file {
                    Some(p) => std::fs::read_to_string(p)
                        .unwrap_or_else(|e| fail(format!("cannot read {}: {e}", p.display()))),
                    None => read_stdin(),
                };
                let findings: Vec<crate::review::Finding> = serde_json::from_str(&raw)
                    .unwrap_or_else(|e| fail(format!("invalid findings JSON: {e}")));
                match crate::review::submit(
                    &conn,
                    &repo,
                    &pr,
                    &agent,
                    &findings,
                    crate::claims::now(),
                ) {
                    Ok(n) => println!("submitted {n} finding(s) as {agent} for {repo}#{pr}"),
                    Err(e) => fail(format!("submit failed: {e}")),
                }
            }
            ReviewAction::Consensus {
                pr,
                base,
                head,
                repo,
                json,
            } => {
                let repo = require_repo(repo);
                let pr = resolve_pr(pr);
                let findings = crate::review::load(&conn, &repo, &pr)
                    .unwrap_or_else(|e| fail(format!("load failed: {e}")));
                let diff = crate::review::compute_diff(base.as_deref(), head.as_deref())
                    .unwrap_or_else(|e| fail(e));
                let changed = crate::review::changed_lines(&diff);
                let items = crate::review::consensus(&findings, &changed);
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&items).unwrap_or_default()
                    );
                } else {
                    println!("{}", crate::review::render_markdown(&items));
                }
            }
            ReviewAction::List { pr, repo } => {
                let repo = require_repo(repo);
                let pr = resolve_pr(pr);
                match crate::review::load(&conn, &repo, &pr) {
                    Ok(fs) if fs.is_empty() => println!("no findings for {repo}#{pr}"),
                    Ok(fs) => {
                        for sf in fs {
                            println!(
                                "{}  {}:{}  {}",
                                sf.agent, sf.finding.file, sf.finding.line, sf.finding.message
                            );
                        }
                    }
                    Err(e) => fail(format!("list failed: {e}")),
                }
            }
            ReviewAction::Clear { pr, repo } => {
                let repo = require_repo(repo);
                let pr = resolve_pr(pr);
                match crate::review::clear(&conn, &repo, &pr) {
                    Ok(n) => println!("cleared {n} finding(s) for {repo}#{pr}"),
                    Err(e) => fail(format!("clear failed: {e}")),
                }
            }
            ReviewAction::Record {
                pr,
                base,
                head,
                repo,
            } => {
                let repo = require_repo(repo);
                let pr = resolve_pr(pr);
                let findings = crate::review::load(&conn, &repo, &pr)
                    .unwrap_or_else(|e| fail(format!("load failed: {e}")));
                let diff = crate::review::compute_diff(base.as_deref(), head.as_deref())
                    .unwrap_or_else(|e| fail(e));
                let changed = crate::review::changed_lines(&diff);
                match crate::review::record_round(
                    &conn,
                    &repo,
                    &pr,
                    &findings,
                    &changed,
                    crate::claims::now(),
                ) {
                    Ok(n) => println!("recorded accuracy for {n} agent(s) on {repo}#{pr}"),
                    Err(e) => fail(format!("record failed: {e}")),
                }
            }
            ReviewAction::Scores {
                repo,
                all_repos,
                json,
            } => {
                let scope = if all_repos {
                    None
                } else {
                    Some(require_repo(repo))
                };
                let scores = crate::review::scores(&conn, scope.as_deref())
                    .unwrap_or_else(|e| fail(format!("scores failed: {e}")));
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&scores).unwrap_or_default()
                    );
                } else if scores.is_empty() {
                    println!("no recorded scores");
                } else {
                    for s in scores {
                        println!(
                            "{:<20} {:.0}%  ({}/{} verified, {} round(s))",
                            s.agent,
                            s.accuracy * 100.0,
                            s.verified,
                            s.findings,
                            s.rounds
                        );
                    }
                }
            }
            ReviewAction::Performance {
                project,
                agent,
                repo,
                days,
                json,
            } => {
                let agent = agent.unwrap_or_else(crate::review::submitter_name);
                let repo = repo.or_else(|| crate::claims::resolve_repo(None));
                let now = crate::claims::now();
                let since = now - days.max(1) * 86_400;

                let backend_conn =
                    match agentflare_backend::db::open_db(&crate::vent::paths::backend_db_path())
                    {
                        Ok(c) => c,
                        Err(e) => fail(format!("cannot open backend db: {e}")),
                    };

                let today = chrono::Local::now().date_naive();
                let cost_start = today - chrono::Duration::days(days.max(1) - 1);
                let cost_totals =
                    crate::cost::summarize((cost_start, today), crate::cost::GroupBy::Project);
                let cost_key = crate::mcp_server::AgentflareMcp::resolve_project_name();
                let project_cost_usd =
                    cost_totals.get(&cost_key).map(|t| t.cost_usd).unwrap_or(0.0);

                let review = match crate::review::performance_review(
                    &backend_conn,
                    &conn,
                    &project,
                    repo.as_deref(),
                    &agent,
                    since,
                    now,
                    project_cost_usd,
                ) {
                    Ok(r) => r,
                    Err(e) => fail(format!("performance_review failed: {e}")),
                };

                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&review).unwrap_or_default()
                    );
                } else {
                    println!("{agent} — project {project} — last {days}d");
                    println!("  completed:  {}", review.quantity_completed);
                    match review.quality_accuracy {
                        Some(acc) => println!(
                            "  quality:    {:.0}% ({}/{} verified, {} round(s), all-time)",
                            acc * 100.0,
                            review.quality_findings,
                            review.quality_findings,
                            review.quality_rounds
                        ),
                        None => println!("  quality:    no recorded review rounds"),
                    }
                    println!("  attention:  {} ask(s)", review.attention_asks);
                    println!(
                        "  cost:       ${:.4} (whole project, all agents, {days}d window)",
                        review.project_cost_usd
                    );
                }

                let mem_conn = match crate::memory::store::open() {
                    Ok(c) => c,
                    Err(e) => fail(format!("cannot open memory store: {e}")),
                };
                let content = serde_json::to_string(&review).unwrap_or_default();
                let topic_key = format!("perf_review:{project}:{agent}");
                match crate::memory::observations::save(
                    &mem_conn,
                    None,
                    "performance_review",
                    &format!("{agent} performance — {project}"),
                    &content,
                    None,
                    Some(&project),
                    Some("workstation"),
                    Some(&topic_key),
                ) {
                    Ok(_) => println!(
                        "\nsaved — run `agentflare memory sync` to share across workstations"
                    ),
                    Err(e) => crate::ui::error(&format!(
                        "warning: review computed but not saved to memory: {e}"
                    )),
                }
            }
        }
    }
}

/// Round id: explicit --pr, else the current branch name.
fn resolve_pr(explicit: Option<String>) -> String {
    explicit.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        flare_git_core::branch::current_branch(&std::env::current_dir().unwrap_or_default())
            .unwrap_or_else(|| fail("could not determine round — pass --pr".to_string()))
    })
}

fn require_repo(explicit: Option<String>) -> String {
    crate::claims::resolve_repo(explicit).unwrap_or_else(|| {
        fail("could not determine repo — run in a git repo or pass --repo owner/name".to_string())
    })
}

fn read_stdin() -> String {
    use std::io::Read;
    let mut s = String::new();
    if std::io::stdin().read_to_string(&mut s).is_err() {
        fail("failed to read findings from stdin".to_string());
    }
    s
}

fn fail(msg: String) -> ! {
    crate::ui::error(&format!("review: {msg}"));
    std::process::exit(1);
}
