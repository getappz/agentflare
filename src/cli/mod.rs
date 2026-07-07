mod agents;
mod alias;
mod auth;
mod coaching;
mod cost;
mod hook;
mod init;
mod mcp;
mod ponytail;
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
    Mcp(mcp::McpArgs),
    Agents(agents::AgentsArgs),
    Alias(alias::AliasArgs),
    Update(update::UpdateArgs),
    Uninstall(uninstall::UninstallArgs),
    Auth(auth::AuthArgs),
    Ponytail(ponytail::PonytailArgs),
}

impl Commands {
    pub fn run(self) {
        match self {
            Self::Init(cmd) => cmd.run(),
            Self::Hook(cmd) => cmd.run(),
            Self::Cost(cmd) => cmd.run(),
            Self::Coaching(cmd) => cmd.run(),
            Self::Mcp(cmd) => cmd.run(),
            Self::Agents(cmd) => cmd.run(),
            Self::Alias(cmd) => cmd.run(),
            Self::Update(cmd) => cmd.run(),
            Self::Uninstall(cmd) => cmd.run(),
            Self::Auth(cmd) => cmd.run(),
            Self::Ponytail(cmd) => cmd.run(),
        }
    }
}
