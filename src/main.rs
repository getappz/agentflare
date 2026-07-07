mod agent_registry;
mod agent_detect;
mod agent_install;
mod agent_launch;
mod agents;
mod alias;
mod auth;
mod auth_crypt;
mod auth_db;
mod auth_runner;
mod build_time;
mod components;
mod coaching;
mod cost;
mod engram_install;
mod errors;
mod hook;
mod init;
mod mcp_server;
mod optimize;
mod paths;
mod ponytail;
mod pricing;
mod rollup;
mod rule_text;
mod shell;
mod state;
mod uninstall;
mod update;

use agent_registry::Agent;
use clap::{Parser, Subcommand};
use std::sync::LazyLock;

static AGENTFLARE_VERSION: LazyLock<String> = LazyLock::new(|| {
    let build_time_str = build_time::BUILD_TIME.format("%Y-%m-%d");
    format!(
        "{} {} ({build_time_str})",
        env!("CARGO_PKG_VERSION"),
        build_time::TARGET,
    )
});

#[derive(Parser)]
#[command(name = "agentflare", version = AGENTFLARE_VERSION.as_str(), about = "Optimize AI CLI agents for cost and performance")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Set up agentflare for one agent: writes rules, installs lean-ctx/engram
    /// (and Ponytail/Caveman on Claude Code), wires the hook config directly
    /// where possible. Running this command is the consent.
    Init {
        #[arg(long, value_enum)]
        agent: Agent,
        /// Skip all prompts — accept defaults.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Hook entry points, invoked by whatever `init` (or the Codex plugin
    /// manifest) wired into the target agent's hook config. Not meant to be
    /// run by hand.
    Hook {
        #[command(subcommand)]
        event: HookEvent,
    },
    /// Print Claude Code token usage and estimated cost, by model or by project.
    Cost {
        /// Widen the window from today to the last N days (inclusive of today). Omit for today only.
        #[arg(long)]
        days: Option<u32>,
        /// Group totals by project instead of by model.
        #[arg(long)]
        by_project: bool,
    },
    /// Manage local coaching rules surfaced alongside built-in nudges.
    Coaching {
        #[command(subcommand)]
        action: CoachingAction,
    },
    /// Start an MCP (Model Context Protocol) server on stdio,
    /// exposing agentflare optimization state as resources and tools.
    Mcp,
    /// Detect installed AI coding agents and show their versions.
    Agents {
        #[command(subcommand)]
        action: AgentsAction,
    },
    /// Set up a shell alias (e.g. af) for agentflare with collision detection
    /// and managed-block persistence. First free alias from the fallback chain
    /// af → agf → afl → agentf → agentflare wins; --force bypasses.
    Alias {
        /// Desired alias name (default: af)
        preferred: Option<String>,
        /// Use exact alias even if occupied
        #[arg(long)]
        force: bool,
        /// Print shell snippet without editing files
        #[arg(long)]
        print: bool,
        /// Skip prompts (installer usage)
        #[arg(long)]
        yes: bool,
        /// Override auto-detected shell (bash, zsh, fish, powershell)
        #[arg(long)]
        shell: Option<String>,
        /// Override target profile file path
        #[arg(long)]
        profile: Option<String>,
        /// Machine-readable output for scripting
        #[arg(long)]
        json: bool,
    },
    /// Self-update agentflare from GitHub Releases. Downloads the latest
    /// (or a specific version), verifies the SHA256 checksum, and replaces
    /// the running binary.
    Update {
        /// Install a specific tagged release instead of latest.
        version: Option<String>,
        /// Check for a newer version without installing.
        #[arg(long)]
        check: bool,
        /// Minimal output.
        #[arg(long)]
        quiet: bool,
    },
    /// Remove everything agentflare init wrote, plus the binary.
    /// Surgical block removal from shared config files — never deletes
    /// whole files that may contain user content.
    Uninstall {
        /// Print what would be removed without touching anything.
        #[arg(long)]
        dry_run: bool,
        /// Leave rules/hooks/MCP config in place, only remove ~/.agentflare/.
        #[arg(long)]
        keep_config: bool,
        /// Don't remove the installed binary.
        #[arg(long)]
        keep_binary: bool,
    },
    /// Auth profile vault — backup, switch, rotate, and manage agent OAuth tokens.
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Manage Ponytail — lazy senior dev mode for AI agents.
    Ponytail {
        #[command(subcommand)]
        action: PonytailAction,
    },
}

#[derive(Subcommand)]
enum CoachingAction {
    /// List all active coaching rules.
    List,
    /// Add or update a coaching rule.
    Apply {
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: String,
    },
    /// Remove a coaching rule.
    Remove { id: String },
}

#[derive(Subcommand)]
enum AgentsAction {
    /// List installed AI coding agents with version and status.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Health check across all installed agents with error details.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Install an agent via its package manager (npm, pip, etc.).
    Install {
        agent: String,
        /// Print the install command without executing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Update an agent to the latest version.
    Update {
        agent: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Uninstall an agent.
    Uninstall {
        agent: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Launch an agent with optional model/mode and pass-through args.
    Launch {
        agent: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        /// Arguments passed through to the agent binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    /// Save current auth files to vault.
    Backup {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
    /// Restore auth files from vault (<100ms switch).
    Activate {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
        /// Restart agent daemon after auth restore (picks up new auth).
        #[arg(long)]
        reload_daemon: bool,
    },
    /// Show active profile via content-hash detection.
    Status {
        /// Limit to one agent.
        agent: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List supported agents for auth vault.
    Catalog {
        #[arg(long)]
        json: bool,
    },
    /// List saved profiles for an agent.
    Ls {
        agent: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove live auth files (logout state).
    Clear {
        agent: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove profile from vault.
    Delete {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
    /// Rename profile (non-destructive).
    Rename {
        agent: String,
        old: String,
        new: String,
        #[arg(long)]
        json: bool,
    },
    /// Smart profile rotation (skips cooldown'd profiles).
    Rotate {
        agent: String,
        /// Rotation algorithm (smart, round-robin, random).
        #[arg(long, default_value = "smart")]
        algorithm: String,
        #[arg(long)]
        json: bool,
    },
    /// Preview what rotation would pick.
    Next {
        agent: String,
        #[arg(long, default_value = "smart")]
        algorithm: String,
        #[arg(long)]
        json: bool,
    },
    /// Interactive profile selector.
    Pick {
        agent: String,
    },
    /// Manage cooldowns.
    Cooldown {
        #[command(subcommand)]
        action: CooldownAction,
    },
    /// Create short alias for a profile.
    Alias {
        agent: String,
        profile: String,
        alias: String,
        #[arg(long)]
        json: bool,
    },
    /// Manage project-profile associations.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Wrap CLI with auto-failover on rate limit. Rotates profiles automatically.
    Run {
        agent: String,
        #[arg(long)]
        json: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Manage isolated $HOME profiles for parallel sessions.
    Isolate {
        #[command(subcommand)]
        action: IsolateAction,
    },
    /// Run command with an isolated profile's $HOME.
    Exec {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Login flow for an isolated profile.
    Login {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PonytailAction {
    /// Show active ponytail mode (reads flag file + config default).
    Status,
    /// Set session-scoped mode (off|lite|full|ultra). Writes flag file.
    Set {
        mode: String,
    },
    /// Persist default mode to config. Survives session restarts.
    Default {
        mode: String,
    },
    /// Turn ponytail off for this session.
    Off,
    /// Re-download SKILL.md from ponytail repo to cache.
    Update,
    /// Run ponytail-review over-engineering code review.
    Review,
    /// Whole-repo audit for over-engineering.
    Audit,
    /// Harvest ponytail: comments into a debt ledger.
    Debt,
    /// Show ponytail benchmark impact stats.
    Gain,
    /// Quick-reference card for all ponytail modes and commands.
    Info,
    /// Hook entry point — called by agent hook systems. Not for manual use.
    Hook {
        #[command(subcommand)]
        event: PonytailHookEvent,
    },
}

#[derive(Subcommand)]
enum PonytailHookEvent {
    /// Session start — emit rules as hook context, write flag file.
    SessionStart,
    /// Subagent start — emit rules for subagent context only.
    SubagentStart,
    /// Prompt submit — parse input for mode switch, update flag if found.
    PromptSubmit,
    /// Output ANSI mode badge for terminal statusline.
    Statusline,
}

#[derive(Subcommand)]
enum IsolateAction {
    /// Create isolated $HOME profile with symlinked host files.
    Add {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
        /// Shallow mode: only auth files copied, everything else symlinked from host.
        #[arg(long)]
        shallow: bool,
    },
    /// List isolated profiles.
    Ls {
        agent: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Delete isolated profile.
    Delete {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum CooldownAction {
    /// Block a profile from rotation for N minutes.
    Set {
        /// <agent>/<profile>
        target: String,
        #[arg(long)]
        minutes: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// List active cooldowns.
    List {
        agent: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Clear a cooldown.
    Clear {
        /// <agent>/<profile>
        target: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// Link current directory to a profile.
    Set {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove project association for current directory.
    Unset {
        agent: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum HookEvent {
    SessionStart {
        #[arg(long, value_enum)]
        agent: Agent,
    },
    PromptSubmit {
        #[arg(long, value_enum)]
        agent: Agent,
    },
    PreToolUse {
        #[arg(long, value_enum)]
        agent: Agent,
    },
}

fn main() {
    color_eyre::install().expect("color_eyre::install failed");
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { agent, yes } => init::run(agent.as_str(), yes),
        Commands::Hook { event } => match event {
            HookEvent::SessionStart { agent } => hook::session_start(agent.as_str()),
            HookEvent::PromptSubmit { agent } => hook::prompt_submit(agent.as_str()),
            HookEvent::PreToolUse { agent } => hook::pre_tool_use(agent.as_str()),
        },
        Commands::Cost { days, by_project } => cost::run(days, by_project),
        Commands::Coaching { action } => match action {
            CoachingAction::List => coaching::print_list(),
            CoachingAction::Apply { id, title, body } => coaching::cli_apply(&id, &title, &body),
            CoachingAction::Remove { id } => coaching::cli_remove(&id),
        },
        Commands::Mcp => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime for mcp server");
            if let Err(e) = runtime.block_on(mcp_server::run()) {
                eprintln!("agentflare mcp: {e}");
                std::process::exit(1);
            }
        }
        Commands::Agents { action } => match action {
            AgentsAction::List { json } => agents::cli_list(json),
            AgentsAction::Doctor { json } => agents::cli_doctor(json),
            AgentsAction::Install { agent, dry_run } => agents::cli_install(&agent, dry_run),
            AgentsAction::Update { agent, dry_run } => agents::cli_update(&agent, dry_run),
            AgentsAction::Uninstall { agent, dry_run } => agents::cli_uninstall(&agent, dry_run),
            AgentsAction::Launch { agent, model, mode, args } => {
                agents::cli_launch(&agent, model.as_deref(), mode.as_deref(), &args)
            }
        },
        Commands::Auth { action } => match action {
            AuthAction::Backup { agent, profile, json } => auth::backup(&agent, &profile, json),
            AuthAction::Activate { agent, profile, json, reload_daemon } => auth::activate_with(&agent, &profile, reload_daemon, json),
            AuthAction::Status { agent, json } => auth::status(agent.as_deref(), json),
            AuthAction::Catalog { json } => auth::list_agents(json),
            AuthAction::Ls { agent, json } => auth::ls(&agent, json),
            AuthAction::Clear { agent, json } => auth::clear(&agent, json),
            AuthAction::Delete { agent, profile, json } => auth::delete(&agent, &profile, json),
            AuthAction::Rename { agent, old, new, json } => auth::rename(&agent, &old, &new, json),
            AuthAction::Rotate { agent, algorithm, json } => auth::rotate(&agent, &algorithm, json),
            AuthAction::Next { agent, algorithm, json } => auth::next(&agent, &algorithm, json),
            AuthAction::Pick { agent } => auth::pick(&agent),
            AuthAction::Cooldown { action } => match action {
                CooldownAction::Set { target, minutes, json } => auth::cooldown_set(&target, minutes, json),
                CooldownAction::List { agent, json } => auth::cooldown_list(agent.as_deref(), json),
                CooldownAction::Clear { target, json } => auth::cooldown_clear(&target, json),
            },
            AuthAction::Alias { agent, profile, alias, json } => auth::set_alias_cmd(&agent, &profile, &alias, json),
            AuthAction::Project { action } => match action {
                ProjectAction::Set { agent, profile, json } => auth::project_set(&agent, &profile, json),
                ProjectAction::Unset { agent, json } => auth::project_unset(&agent, json),
            },
            AuthAction::Run { agent, json, args } => auth_runner::run(&agent, &args, json),
            AuthAction::Isolate { action } => match action {
                IsolateAction::Add { agent, profile, json, shallow } => auth::isolate_add_with(&agent, &profile, shallow, json),
                IsolateAction::Ls { agent, json } => auth::isolate_ls(agent.as_deref(), json),
                IsolateAction::Delete { agent, profile, json } => auth::isolate_delete(&agent, &profile, json),
            },
            AuthAction::Exec { agent, profile, json, args } => auth::auth_exec(&agent, &profile, &args, json),
            AuthAction::Login { agent, profile, json, args } => auth::auth_login(&agent, &profile, &args, json),
        },
        Commands::Alias { preferred, force, print, yes, shell, profile, json } => {
            alias::run(preferred, force, print, yes, shell, profile, json)
        }
        Commands::Update { version, check, quiet } => update::run(version, check, quiet),
        Commands::Uninstall { dry_run, keep_config, keep_binary } => {
            uninstall::run(dry_run, keep_config, keep_binary)
        }
        Commands::Ponytail { action } => match action {
            PonytailAction::Status => {
                let mode = ponytail::active_mode().unwrap_or_else(ponytail::default_mode);
                println!("{mode}");
            }
            PonytailAction::Set { mode } => {
                let normalized = ponytail::normalize_config_mode(&mode)
                    .unwrap_or("full");
                ponytail::set_active(normalized).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                });
                println!("{normalized}");
            }
            PonytailAction::Default { mode } => {
                ponytail::set_default_mode(&mode).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                });
                ponytail::set_active(&mode).ok();
                println!("default: {mode}");
            }
            PonytailAction::Off => {
                ponytail::clear_active();
                println!("off");
            }
            PonytailAction::Update => {
                match ponytail::download_skill() {
                    Ok(path) => println!("SKILL.md updated at {path}"),
                    Err(e) => {
                        eprintln!("update failed: {e}");
                        std::process::exit(1);
                    }
                }
            }
            PonytailAction::Review => {
                println!("{}", ponytail::sub_skills::SKILL_REVIEW);
            }
            PonytailAction::Audit => {
                println!("{}", ponytail::sub_skills::SKILL_AUDIT);
            }
            PonytailAction::Debt => {
                println!("{}", ponytail::sub_skills::SKILL_DEBT);
            }
            PonytailAction::Gain => {
                println!("{}", ponytail::sub_skills::SKILL_GAIN);
            }
            PonytailAction::Info => {
                println!("{}", ponytail::sub_skills::SKILL_HELP);
            }
            PonytailAction::Hook { event } => match event {
                PonytailHookEvent::SessionStart => {
                    let mode = ponytail::active_mode()
                        .unwrap_or_else(ponytail::default_mode);
                    if mode == "off" {
                        ponytail::state::clear_active();
                        println!("OK");
                        return;
                    }
                    ponytail::set_active(&mode).ok();
                    let instructions = ponytail::build_instructions(&mode, None);
                    let platform = ponytail::detect_platform();
                    let output = ponytail::format_hook_output(
                        "SessionStart",
                        &instructions.body,
                        &platform,
                    );
                    println!("{output}");
                }
                PonytailHookEvent::SubagentStart => {
                    let mode = ponytail::active_mode()
                        .unwrap_or_else(ponytail::default_mode);
                    if mode == "off" {
                        println!("OK");
                        return;
                    }
                    let instructions = ponytail::build_instructions(&mode, None);
                    let platform = ponytail::detect_platform();
                    let output = ponytail::format_hook_output(
                        "SubagentStart",
                        &instructions.body,
                        &platform,
                    );
                    println!("{output}");
                }
                PonytailHookEvent::PromptSubmit => {
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).ok();
                    if let Some(action) = ponytail::detect_switch(&input) {
                        match action {
                            ponytail::SwitchAction::SetMode(m) => {
                                ponytail::set_active(&m).ok();
                            }
                            ponytail::SwitchAction::SetDefault(m) => {
                                ponytail::set_default_mode(&m).ok();
                                ponytail::set_active(&m).ok();
                            }
                            ponytail::SwitchAction::Off => {
                                ponytail::clear_active();
                            }
                        }
                    }
                    println!("OK");
                }
                PonytailHookEvent::Statusline => {
                    let mode = ponytail::active_mode()
                        .unwrap_or_else(ponytail::default_mode);
                    if mode == "off" || mode.is_empty() {
                        return;
                    }
                    if mode == "full" {
                        print!("\x1b[38;5;108m[PONYTAIL]\x1b[0m");
                    } else {
                        let upper = mode.to_uppercase();
                        print!("\x1b[38;5;108m[PONYTAIL:{upper}]\x1b[0m");
                    }
                }
            },
        }
    }
}
