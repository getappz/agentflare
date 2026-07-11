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
}

impl ReviewArgs {
    pub fn run(self) {
        let conn = match crate::db::open() {
            Ok(c) => c,
            Err(e) => fail(format!("cannot open ledger: {e}")),
        };
        match self.action {
            ReviewAction::Submit { pr, agent, file, repo } => {
                let repo = require_repo(repo);
                let pr = resolve_pr(pr);
                let agent = agent.unwrap_or_else(crate::review::submitter_name);
                let raw = match &file {
                    Some(p) => std::fs::read_to_string(p).unwrap_or_else(|e| fail(format!("cannot read {}: {e}", p.display()))),
                    None => read_stdin(),
                };
                let findings: Vec<crate::review::Finding> = serde_json::from_str(&raw)
                    .unwrap_or_else(|e| fail(format!("invalid findings JSON: {e}")));
                match crate::review::submit(&conn, &repo, &pr, &agent, &findings, crate::claims::now()) {
                    Ok(n) => println!("submitted {n} finding(s) as {agent} for {repo}#{pr}"),
                    Err(e) => fail(format!("submit failed: {e}")),
                }
            }
            ReviewAction::Consensus { pr, base, head, repo, json } => {
                let repo = require_repo(repo);
                let pr = resolve_pr(pr);
                let findings = crate::review::load(&conn, &repo, &pr)
                    .unwrap_or_else(|e| fail(format!("load failed: {e}")));
                let diff = crate::review::compute_diff(base.as_deref(), head.as_deref())
                    .unwrap_or_else(|e| fail(e));
                let changed = crate::review::changed_lines(&diff);
                let items = crate::review::consensus(&findings, &changed);
                if json {
                    println!("{}", serde_json::to_string_pretty(&items).unwrap_or_default());
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
                            println!("{}  {}:{}  {}", sf.agent, sf.finding.file, sf.finding.line, sf.finding.message);
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
        }
    }
}

/// Round id: explicit --pr, else the current branch name.
fn resolve_pr(explicit: Option<String>) -> String {
    explicit.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| fail("could not determine round — pass --pr".to_string()))
    })
}

fn require_repo(explicit: Option<String>) -> String {
    crate::claims::resolve_repo(explicit)
        .unwrap_or_else(|| fail("could not determine repo — run in a git repo or pass --repo owner/name".to_string()))
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
    eprintln!("review: {msg}");
    std::process::exit(1);
}
