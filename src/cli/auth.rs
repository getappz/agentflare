use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum AuthAction {
    Backup {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
    Activate {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        reload_daemon: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
        agent: Option<String>,
    },
    Catalog {
        #[arg(long)]
        json: bool,
    },
    Ls {
        agent: String,
        #[arg(long)]
        json: bool,
    },
    Clear {
        agent: String,
        #[arg(long)]
        json: bool,
    },
    Delete {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
    Rename {
        agent: String,
        old: String,
        new: String,
        #[arg(long)]
        json: bool,
    },
    Rotate {
        agent: String,
        #[arg(long, default_value = "smart")]
        algorithm: String,
        #[arg(long)]
        json: bool,
    },
    Next {
        agent: String,
        #[arg(long, default_value = "smart")]
        algorithm: String,
        #[arg(long)]
        json: bool,
    },
    Pick {
        agent: String,
    },
    Cooldown {
        #[command(subcommand)]
        action: CooldownAction,
    },
    Alias {
        agent: String,
        profile: String,
        alias: String,
        #[arg(long)]
        json: bool,
    },
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    Run {
        agent: String,
        #[arg(long)]
        json: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Isolate {
        #[command(subcommand)]
        action: IsolateAction,
    },
    Exec {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
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
    Set {
        target: String,
        #[arg(long)]
        minutes: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
        agent: Option<String>,
    },
    Clear {
        target: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ProjectAction {
    Set {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
    Unset {
        agent: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum IsolateAction {
    Add {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        shallow: bool,
    },
    Ls {
        #[arg(long)]
        json: bool,
        agent: Option<String>,
    },
    Delete {
        agent: String,
        profile: String,
        #[arg(long)]
        json: bool,
    },
}

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
