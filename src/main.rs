mod agent_install;
mod agent_launch;
mod agents;
mod alias;
mod artifacts;
mod auth;
mod auth_crypt;
mod auth_db;
mod auth_runner;
mod build_time;
mod gateway_secrets;
mod cli;
mod coaching;
mod components;
mod config;
mod dev_vars;
mod cost;
mod engram_install;
mod errors;
mod hook;
mod init;
mod mcp_prompts;
mod mcp_server;
mod mise_install;
mod optimize;
mod paths;
mod pricing;
mod rollup;
mod rule_text;
mod shell;
mod state;
mod tool_install;
mod uninstall;
mod update;

use clap::Parser;

fn main() {
    color_eyre::install().expect("color_eyre::install failed");
    let cli = cli::Cli::parse();
    cli.command.run();
}
