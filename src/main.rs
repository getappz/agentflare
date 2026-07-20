mod about;
mod agent_install;
mod agent_launch;
mod agents;
mod alias;
mod asset_store;
mod artifacts;
mod atomic_fs;
mod auth;
mod auth_crypt;
mod auth_db;
mod auth_runner;
mod banner;
mod build_time;
mod channels;
mod claims;
mod cli;
mod coaching;
mod compact;
mod components;
mod core;
mod cost;
mod daemon;
mod daemon_autostart;
mod daemon_client;
mod dashboard;
mod db;
mod dev_install;
mod dev_vars;
mod errors;
mod gateway_integrations;
mod gateway_secrets;
mod git;
mod github;
mod hook;
mod hook_redirect;
mod init;
mod ipc;
mod jsonc;
mod mcp_prompts;
mod mcp_server;
mod memory;
mod mentions;
mod mise_install;
mod optimize;
mod paths;
mod pricing;
mod progress;
mod review;
mod rollup;
mod rule_text;
mod shell;
mod state;
mod store;
mod tool_install;
mod ui;
mod uninstall;
mod update;
mod vent;
mod worktree;

use clap::Parser;

fn main() {
    color_eyre::install().expect("color_eyre::install failed");
    let cli = cli::Cli::parse();
    match cli.command {
        Some(command) => command.run(),
        None => crate::about::run(crate::about::AboutArgs {}),
    }
}
