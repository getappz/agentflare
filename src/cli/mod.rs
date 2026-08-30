mod agents;
mod alias;
mod apps;
mod artifacts;
mod auth;
mod channel;
mod claim;
mod coaching;
mod code;
mod config;
mod cost;
mod daemon;
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
mod mcp;
mod memory;
mod optimize;
mod review;
mod run;
mod serve;
mod skill;
mod uninstall;
mod update;
mod vault;
mod vent;
pub(crate) mod work;
mod workflow;

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
    /// Update agentflare to the latest release.
    Update(update::UpdateArgs),
    /// Remove agentflare's hooks, config, and installed files.
    Uninstall(uninstall::UninstallArgs),
    /// Manage secrets stored in agentflare's encrypted vault.
    Vault(vault::VaultArgs),
    /// Manage authentication credentials for connected services.
    Auth(auth::AuthArgs),
    /// Serve live-shareable artifact pages from AI agent sessions.
    Artifacts(artifacts::ArtifactsArgs),
    /// Hand a work product to another agent's inbox.
    Handoff(handoff::HandoffArgs),
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
    /// Review a diff, PR, or branch for correctness and simplification.
    Review(review::ReviewArgs),
    /// Discover, install, and manage skills for coding agents.
    Skill(skill::SkillArgs),
    /// Read and write agentflare's cross-session agent memory.
    Memory(memory::MemoryArgs),
    /// Serve the read-only agentflare dashboard.
    Serve(serve::ServeArgs),
    /// Vent friction or feedback encountered during an agent session.
    Vent(vent::VentArgs),
    /// Manage work items in the agentflare project queue.
    Work(work::WorkArgs),
    /// Search and fetch cached third-party API documentation.
    Docs(docs::DocsArgs),
    /// Run and inspect durable agent pipelines through the workflow engine.
    Workflow(workflow::WorkflowArgs),
    /// Unified observability for AI coding sessions (Claude/Codex/OpenCode/Cursor/Gemini) — local-first.
    Insights(insights::InsightsArgs),
    /// Run and manage AgentFlare Apps — self-contained agentic domain modules.
    Apps(apps::AppsArgs),
}

impl Commands {
    pub fn run(self) {
        match self {
            Self::Init(cmd) => cmd.run(),
            Self::Hook(cmd) => cmd.run(),
            Self::Cost(cmd) => cmd.run(),
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
            Self::Update(cmd) => cmd.run(),
            Self::Uninstall(cmd) => cmd.run(),
            Self::Vault(cmd) => cmd.run(),
            Self::Auth(cmd) => cmd.run(),
            Self::Artifacts(cmd) => cmd.run(),
            Self::Handoff(cmd) => cmd.run(),
            Self::GithubBridge(cmd) => cmd.run(),
            Self::Optimize(cmd) => cmd.run(),
            Self::About(cmd) => crate::about::run(cmd),
            Self::Channel(cmd) => cmd.run(),
            Self::Claim(cmd) => cmd.run(),
            Self::Review(cmd) => cmd.run(),
            Self::Skill(cmd) => cmd.run(),
            Self::Memory(cmd) => cmd.run(),
            Self::Serve(cmd) => cmd.run(),
            Self::Daemon(cmd) => cmd.run(),
            Self::Vent(cmd) => vent::run(cmd),
            Self::Work(cmd) => cmd.run(),
            Self::Docs(cmd) => docs::run(cmd),
            Self::Workflow(cmd) => cmd.run(),
            Self::Insights(cmd) => cmd.run(),
            Self::Apps(cmd) => cmd.run(),
        }
    }
}
