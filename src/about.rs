//! `agentflare about` — show the branding banner, version, and where to go next.
//!
//! Also the target of a bare `agentflare` invocation (no subcommand), per the
//! "invisible once configured" design: the splash appears on explicit request,
//! never on the stdio/MCP path.

use clap::Args;

use crate::banner::print_banner;
use crate::cli::AGENTFLARE_VERSION;

#[derive(Args)]
#[command(about = "Show agentflare version and branding")]
pub struct AboutArgs {}

pub fn run(_args: AboutArgs) {
    print_banner();
    println!();
    println!("  version {}", AGENTFLARE_VERSION.as_str());
    println!();
    println!(
        "  Get started: agentflare init --agent <claude-code|codex|cursor|windsurf|vscode-copilot|cline|continue>"
    );
    println!("  Docs:       https://github.com/getappz/agentflare");
}
