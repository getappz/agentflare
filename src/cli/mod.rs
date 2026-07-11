mod agents;
mod alias;
mod artifacts;
mod auth;
mod caveman;
mod claim;
mod coaching;
mod cost;
mod gateway;
mod handoff;
mod hook;
mod init;
mod mcp;
mod ponytail;
mod review;
mod run;
mod uninstall;
mod update;

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

#[derive(Parser)]
#[command(name = "agentflare", version = AGENTFLARE_VERSION.as_str(), about = "Optimize AI CLI agents for cost and performance")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    Init(init::InitArgs),
    Hook(hook::HookArgs),
    Cost(cost::CostArgs),
    Coaching(coaching::CoachingArgs),
    Gateway(gateway::GatewayArgs),
    Mcp(mcp::McpArgs),
    Agents(agents::AgentsArgs),
    Run(run::RunArgs),
    Alias(alias::AliasArgs),
    Update(update::UpdateArgs),
    Uninstall(uninstall::UninstallArgs),
    Auth(auth::AuthArgs),
    Artifacts(artifacts::ArtifactsArgs),
    Handoff(handoff::HandoffArgs),
    Ponytail(ponytail::PonytailArgs),
    Caveman(caveman::CavemanArgs),
    Claim(claim::ClaimArgs),
    Review(review::ReviewArgs),
}

impl Commands {
    pub fn run(self) {
        match self {
            Self::Init(cmd) => cmd.run(),
            Self::Hook(cmd) => cmd.run(),
            Self::Cost(cmd) => cmd.run(),
            Self::Coaching(cmd) => cmd.run(),
            Self::Gateway(cmd) => cmd.run(),
            Self::Mcp(cmd) => cmd.run(),
            Self::Agents(cmd) => cmd.run(),
            Self::Run(cmd) => cmd.run(),
            Self::Alias(cmd) => cmd.run(),
            Self::Update(cmd) => cmd.run(),
            Self::Uninstall(cmd) => cmd.run(),
            Self::Auth(cmd) => cmd.run(),
            Self::Artifacts(cmd) => cmd.run(),
            Self::Handoff(cmd) => cmd.run(),
            Self::Ponytail(cmd) => cmd.run(),
            Self::Caveman(cmd) => cmd.run(),
            Self::Claim(cmd) => cmd.run(),
            Self::Review(cmd) => cmd.run(),
        }
    }
}
