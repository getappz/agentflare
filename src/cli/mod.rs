mod agents;
mod alias;
mod apps;
mod artifacts;
mod auth;
mod browser;
mod channel;
mod claim;
mod clean;
mod coaching;
mod code;
mod config;
mod cost;
mod daemon;
mod decide;
mod dev_install;
mod docs;
mod doctor;
mod gateway;
pub(crate) mod git;
mod github_bridge;
mod handoff;
mod hook;
mod init;
mod insights;
mod item;
mod job;
mod mcp;
mod memory;
mod message;
mod optimize;
mod pr;
mod project;
mod review;
mod run;
mod serve;
mod skill;
mod store;
mod uninstall;
mod update;
mod vault;
mod vent;
pub(crate) mod work;
mod workflow;
mod workflow_schedule;
mod wrap;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Parser, Subcommand};
use std::sync::LazyLock;

pub static AGENTFLARE_VERSION: LazyLock<String> = LazyLock::new(|| {
    let build_time_str = crate::build_time::BUILD_TIME.format("%Y-%m-%d");
    format!(
        "{} {} ({build_time_str})",
        env!("CARGO_PKG_VERSION"),
        crate::build_time::TARGET,
    )
});

/// Shared help/usage/error theme, applied to every subcommand's `--help`
/// automatically since clap derives them from the root `Command`.
fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default() | Effects::BOLD)
        .usage(AnsiColor::Yellow.on_default() | Effects::BOLD)
        .literal(AnsiColor::Green.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Cyan.on_default())
        .error(AnsiColor::Red.on_default() | Effects::BOLD)
        .valid(AnsiColor::Green.on_default())
        .invalid(AnsiColor::Yellow.on_default())
}

#[derive(Parser)]
#[command(
    name = "agentflare",
    version = AGENTFLARE_VERSION.as_str(),
    about = "Optimize AI CLI agents for cost and performance",
    styles = cli_styles(),
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Wire agentflare into a coding agent (hooks, MCP server, config).
    Init(init::InitArgs),
    /// Internal hook entry point invoked by an agent's lifecycle events. Not meant for direct use.
    Hook(hook::HookArgs),
    /// Show AI agent token/dollar cost, optionally broken down by project.
    Cost(cost::CostArgs),
    /// Typed-decision layer (Jev): check that the connection works.
    Decide(decide::DecideArgs),
    /// Build the current source tree and install it over the running binary.
    DevInstall(dev_install::DevInstallArgs),
    /// Diagnose agent config wiring and report problems.
    Doctor(doctor::DoctorArgs),
    /// Manage coaching rules nudged to agents mid-session.
    Coaching(coaching::CoachingArgs),
    /// Static analysis for the current repo (currently: change-impact).
    Code(code::CodeArgs),
    /// Manage `~/.agentflare/config.toml` settings that aren't repo-scoped.
    Config(config::ConfigArgs),
    /// Manage credentials and connections for third-party gateways.
    Gateway(gateway::GatewayArgs),
    /// Git helpers for agent worktrees and branch hygiene.
    Git(git::GitArgs),
    /// Manage the agentflare MCP server registration for coding agents.
    Mcp(mcp::McpArgs),
    /// Inspect and manage detected coding-agent installs.
    Agents(agents::AgentsArgs),
    /// Launch an agent through mise with `.dev.vars` env vars injected.
    Run(run::RunArgs),
    /// Manage shell aliases installed for agentflare commands.
    Alias(alias::AliasArgs),
    /// Install a Claude shell wrapper with opt-in --with-proxy support.
    Wrap(wrap::WrapArgs),
    /// Remove agentflare's Claude shell wrapper from a shell profile.
    Unwrap(wrap::WrapArgs),
    /// Update agentflare to the latest release.
    Update(update::UpdateArgs),
    /// Remove agentflare's hooks, config, and installed files.
    Uninstall(uninstall::UninstallArgs),
    /// Manage secrets stored in agentflare's encrypted vault.
    Vault(vault::VaultArgs),
    /// Manage authentication credentials for connected services.
    Auth(auth::AuthArgs),
    /// Agent-first browser automation (open, snapshot, click, fill, read, ...).
    Browser(browser::BrowserArgs),
    /// Serve live-shareable artifact pages from AI agent sessions.
    Artifacts(artifacts::ArtifactsArgs),
    /// Hand a work product to another agent's inbox.
    Handoff(Box<handoff::HandoffArgs>),
    /// Configure the GitHub work-item bridge for this repo.
    GithubBridge(github_bridge::GithubBridgeArgs),
    #[command(alias = "flare", visible_alias = "opt")]
    /// Optimize prompts, context, and instructions for cost and quality.
    Optimize(optimize::OptimizeArgs),
    #[command(visible_alias = "logo")]
    /// Print the agentflare banner and version info.
    About(crate::about::AboutArgs),
    /// Manage the agentflare background daemon (dashboard, bridge, watchers).
    Daemon(daemon::DaemonArgs),
    /// Send and inspect messages on agent coordination channels.
    Channel(channel::ChannelArgs),
    /// Claim a work item for the current agent session.
    Claim(claim::ClaimArgs),
    /// Clean up merged branches, worktrees and build artifacts: shows a plan, then asks.
    #[command(visible_alias = "cleanup")]
    Clean(clean::CleanArgs),
    /// Review a diff, PR, or branch for correctness and simplification.
    Review(review::ReviewArgs),
    /// Discover, install, and manage skills for coding agents.
    Skill(skill::SkillArgs),
    /// Read and write agentflare's cross-session agent memory.
    Memory(memory::MemoryArgs),
    /// Message live agent sessions (send, list, inbox, watch).
    Message(message::MessageArgs),
    /// Serve the read-only agentflare dashboard.
    Serve(serve::ServeArgs),
    /// Vent friction or feedback encountered during an agent session.
    Vent(vent::VentArgs),
    /// Open stamped PRs and manage cross-instance PR ownership.
    Pr(pr::PrArgs),
    /// Manage work items in the agentflare project queue.
    ///
    /// Automatic failover moves an item to another installed agent when the
    /// current one is rate limited, out of credit, over its subscription's
    /// usage threshold, or its CLI invocation is broken. Two item-metadata
    /// keys scope this per item (set via `agentflare item update --metadata`,
    /// not a work flag): `"failover": false` pins the item to its current
    /// assignee_agent, and `"allowed_agents": ["codex", "opencode"]`
    /// restricts any failover move to that list. The usage threshold and the
    /// operator-wide on/off/allow-list are `~/.agentflare/config.toml`'s
    /// `[failover]` table.
    Work(work::WorkArgs),
    /// Search and fetch cached third-party API documentation.
    Docs(docs::DocsArgs),
    /// Run and inspect durable agent pipelines through the workflow engine.
    Workflow(workflow::WorkflowArgs),
    /// Unified observability for AI coding sessions (Claude/Codex/OpenCode) — local-first.
    Insights(insights::InsightsArgs),
    /// Run and manage AgentFlare Apps — self-contained agentic domain modules.
    Apps(apps::AppsArgs),
    /// Manage work items (list/get/cancel/pause/resume/redispatch)
    Item(item::ItemArgs),
    /// Control dispatched jobs (cancel/pause/resume)
    Job(job::JobArgs),
    /// Manage projects
    Project(project::ProjectArgs),
    /// Local store (chunks, vectors, meta, cache) — hybrid search internals.
    Store(store::StoreArgs),
}

impl Commands {
    pub fn run(self) {
        match self {
            Self::Init(cmd) => cmd.run(),
            Self::Hook(cmd) => cmd.run(),
            Self::Cost(cmd) => cmd.run(),
            Self::Decide(cmd) => cmd.run(),
            Self::DevInstall(cmd) => cmd.run(),
            Self::Doctor(cmd) => cmd.run(),
            Self::Coaching(cmd) => cmd.run(),
            Self::Code(cmd) => code::run(cmd),
            Self::Config(cmd) => cmd.run(),
            Self::Gateway(cmd) => cmd.run(),
            Self::Git(cmd) => git::run(cmd),
            Self::Mcp(cmd) => cmd.run(),
            Self::Agents(cmd) => cmd.run(),
            Self::Run(cmd) => cmd.run(),
            Self::Alias(cmd) => cmd.run(),
            Self::Wrap(cmd) => cmd.run(false),
            Self::Unwrap(cmd) => cmd.run(true),
            Self::Update(cmd) => cmd.run(),
            Self::Uninstall(cmd) => cmd.run(),
            Self::Vault(cmd) => cmd.run(),
            Self::Auth(cmd) => cmd.run(),
            Self::Browser(cmd) => cmd.run(),
            Self::Artifacts(cmd) => cmd.run(),
            Self::Handoff(cmd) => cmd.run(),
            Self::GithubBridge(cmd) => cmd.run(),
            Self::Optimize(cmd) => cmd.run(),
            Self::About(cmd) => crate::about::run(cmd),
            Self::Channel(cmd) => cmd.run(),
            Self::Claim(cmd) => cmd.run(),
            Self::Clean(cmd) => clean::run(cmd),
            Self::Review(cmd) => cmd.run(),
            Self::Skill(cmd) => cmd.run(),
            Self::Memory(cmd) => cmd.run(),
            Self::Message(cmd) => cmd.run(),
            Self::Serve(cmd) => cmd.run(),
            Self::Daemon(cmd) => cmd.run(),
            Self::Vent(cmd) => vent::run(cmd),
            Self::Pr(cmd) => cmd.run(),
            Self::Work(cmd) => cmd.run(),
            Self::Docs(cmd) => docs::run(cmd),
            Self::Workflow(cmd) => cmd.run(),
            Self::Insights(cmd) => cmd.run(),
            Self::Apps(cmd) => cmd.run(),
            Self::Item(cmd) => cmd.run(),
            Self::Job(cmd) => cmd.run(),
            Self::Project(cmd) => cmd.run(),
            Self::Store(cmd) => cmd.run(),
        }
    }
}

/// Parse a duration. Units: `s` seconds, `m` minutes, `h` hours, `d` days,
/// `w` weeks, `M` 30-day months. `m` and `M` are distinct. Segments may be
/// concatenated or whitespace-separated (`30s 5m`, `2h30m`). Unitless,
/// negative, unknown units, and overflow are rejected.
pub(crate) fn parse_duration(input: &str) -> Result<std::time::Duration, String> {
    let bad = || format!("invalid duration '{input}' (use e.g. 30s, 5m, 12h, 7d, 2w, 3M)");
    let s = input.trim();
    if s.is_empty() || s.as_bytes().first() == Some(&b'-') {
        return Err(bad());
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut total: u64 = 0;
    let mut saw = false;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if !bytes[i].is_ascii_digit() {
            return Err(bad());
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i >= bytes.len() {
            return Err(bad());
        }
        let unit_secs: u64 = match bytes[i] {
            b's' => 1,
            b'm' => 60,
            b'h' => 3600,
            b'd' => 86_400,
            b'w' => 7 * 86_400,
            b'M' => 30 * 86_400,
            _ => return Err(bad()),
        };
        i += 1;
        let n: u64 = s[start..i - 1].parse().map_err(|_| bad())?;
        let part = n.checked_mul(unit_secs).ok_or_else(bad)?;
        total = total.checked_add(part).ok_or_else(bad)?;
        saw = true;
    }
    if !saw {
        return Err(bad());
    }
    Ok(std::time::Duration::from_secs(total))
}

#[cfg(test)]
mod duration_tests {
    use super::parse_duration;
    use std::time::Duration;

    #[test]
    fn parses_s_m_and_month_distinctly() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(5 * 60));
        assert_eq!(
            parse_duration("1M").unwrap(),
            Duration::from_secs(30 * 86_400)
        );
        assert_ne!(parse_duration("1m").unwrap(), parse_duration("1M").unwrap());
        assert_eq!(
            parse_duration("30s 5m 2h 1d 1w 3M").unwrap(),
            Duration::from_secs(30 + 5 * 60 + 2 * 3600 + 86_400 + 7 * 86_400 + 3 * 30 * 86_400)
        );
        assert_eq!(
            parse_duration("2h30m").unwrap(),
            Duration::from_secs(2 * 3600 + 30 * 60)
        );
        for bad in [
            "soon",
            "",
            "d",
            "7",
            "7x",
            "-1d",
            "5 m",
            "999999999999999999h",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}
