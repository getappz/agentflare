use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum AuthAction {
    /// Snapshot an agent's live credentials into a named profile.
    Backup {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
    /// Make a saved profile the agent's live credentials.
    Activate {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        reload_daemon: bool,
    },
    /// Show which profile is active and its quota/cooldown state.
    Status {
        #[arg(long)]
        json: bool,
        agent: Option<String>,
    },
    /// List every agent kind agentflare knows how to manage credentials for.
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
    /// Clear an agent's live credentials without deleting saved profiles.
    Clear {
        agent: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a saved profile.
    Delete {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
    /// Rename a saved profile.
    Rename {
        agent: String,
        old: String,
        new: String,
        #[arg(long)]
        json: bool,
    },
    /// Switch to the next profile by rotation algorithm and activate it.
    Rotate {
        agent: String,
        #[arg(long, default_value = "smart")]
        algorithm: String,
        #[arg(long)]
        json: bool,
    },
    /// Show which profile rotation would pick next, without activating it.
    Next {
        agent: String,
        #[arg(long, default_value = "smart")]
        algorithm: String,
        #[arg(long)]
        json: bool,
    },
    /// Interactively choose a profile to activate.
    Pick { agent: String },
    /// Manage per-profile rotation cooldowns.
    Cooldown {
        #[command(subcommand)]
        action: CooldownAction,
    },
    /// Give a profile a human-readable alias.
    Alias {
        agent: String,
        profile: String,
        alias: String,
        #[arg(long)]
        json: bool,
    },
    /// Manage which profile a project directory should use by default.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Run the agent's CLI with its currently active credentials.
    Run {
        agent: String,
        #[arg(long)]
        json: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Manage credential isolation (per-profile sandboxed auth state).
    Isolate {
        #[command(subcommand)]
        action: IsolateAction,
    },
    /// Run the agent's CLI under a specific saved profile, without activating it.
    Exec {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the agent's interactive login flow and save the result as a profile.
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
pub enum CooldownAction {
    /// Set a rotation cooldown, in minutes, on a profile.
    Set {
        target: String,
        #[arg(long)]
        minutes: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// List active cooldowns.
    List {
        #[arg(long)]
        json: bool,
        agent: Option<String>,
    },
    /// Clear a profile's cooldown, making it eligible for rotation again.
    Clear {
        target: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ProjectAction {
    /// Pin the current project directory to a profile.
    Set {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
    /// Unpin the current project directory from its profile.
    Unset {
        agent: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum IsolateAction {
    /// Isolate a profile's credential state from the others.
    Add {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        shallow: bool,
    },
    /// List isolated profiles.
    Ls {
        #[arg(long)]
        json: bool,
        agent: Option<String>,
    },
    /// Remove a profile's isolation, folding it back into shared state.
    Delete {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
}

/// Manage per-agent credentials: backup/activate/rotate profiles, run agents
/// under a specific profile, and isolate or pin credentials per project.
#[derive(Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub action: AuthAction,
}

impl AuthArgs {
    pub fn run(self) {
        match self.action {
            AuthAction::Backup {
                agent,
                profile,
                json,
            } => crate::auth::backup(&agent, &profile, json),
            AuthAction::Activate {
                agent,
                profile,
                json,
                reload_daemon,
            } => crate::auth::activate_with(&agent, &profile, reload_daemon, json),
            AuthAction::Status { agent, json } => crate::auth::status(agent.as_deref(), json),
            AuthAction::Catalog { json } => crate::auth::list_agents(json),
            AuthAction::Ls { agent, json } => crate::auth::ls(&agent, json),
            AuthAction::Clear { agent, json } => crate::auth::clear(&agent, json),
            AuthAction::Delete {
                agent,
                profile,
                json,
            } => crate::auth::delete(&agent, &profile, json),
            AuthAction::Rename {
                agent,
                old,
                new,
                json,
            } => crate::auth::rename(&agent, &old, &new, json),
            AuthAction::Rotate {
                agent,
                algorithm,
                json,
            } => crate::auth::rotate(&agent, &algorithm, json),
            AuthAction::Next {
                agent,
                algorithm,
                json,
            } => crate::auth::next(&agent, &algorithm, json),
            AuthAction::Pick { agent } => crate::auth::pick(&agent),
            AuthAction::Cooldown { action } => match action {
                CooldownAction::Set {
                    target,
                    minutes,
                    json,
                } => crate::auth::cooldown_set(&target, minutes, json),
                CooldownAction::List { agent, json } => {
                    crate::auth::cooldown_list(agent.as_deref(), json)
                }
                CooldownAction::Clear { target, json } => {
                    crate::auth::cooldown_clear(&target, json)
                }
            },
            AuthAction::Alias {
                agent,
                profile,
                alias,
                json,
            } => crate::auth::set_alias_cmd(&agent, &profile, &alias, json),
            AuthAction::Project { action } => match action {
                ProjectAction::Set {
                    agent,
                    profile,
                    json,
                } => crate::auth::project_set(&agent, &profile, json),
                ProjectAction::Unset { agent, json } => crate::auth::project_unset(&agent, json),
            },
            AuthAction::Run { agent, json, args } => crate::auth_runner::run(&agent, &args, json),
            AuthAction::Isolate { action } => match action {
                IsolateAction::Add {
                    agent,
                    profile,
                    json,
                    shallow,
                } => crate::auth::isolate_add_with(&agent, &profile, shallow, json),
                IsolateAction::Ls { agent, json } => {
                    crate::auth::isolate_ls(agent.as_deref(), json)
                }
                IsolateAction::Delete {
                    agent,
                    profile,
                    json,
                } => crate::auth::isolate_delete(&agent, &profile, json),
            },
            AuthAction::Exec {
                agent,
                profile,
                json,
                args,
            } => crate::auth::auth_exec(&agent, &profile, &args, json),
            AuthAction::Login {
                agent,
                profile,
                json,
                args,
            } => crate::auth::auth_login(&agent, &profile, &args, json),
        }
    }
}
