use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum GatewayAction {
    Secret {
        #[command(subcommand)]
        action: GatewaySecretAction,
    },
}

#[derive(Subcommand)]
pub enum GatewaySecretAction {
    /// Set a secret's value, read from stdin (never as a CLI argument, so it
    /// never lands in shell history).
    Set { name: String },
    /// List the names of stored secrets (never their values).
    List,
    /// Remove a stored secret.
    Remove { name: String },
}

#[derive(Args)]
pub struct GatewayArgs {
    #[command(subcommand)]
    pub action: GatewayAction,
}

impl GatewayArgs {
    pub fn run(self) {
        match self.action {
            GatewayAction::Secret { action } => run_secret(action),
        }
    }
}

fn run_secret(action: GatewaySecretAction) {
    let conn = match crate::db::open() {
        Ok(c) => c,
        Err(e) => {
            crate::ui::error(&format!("failed to open agentflare.db: {e}"));
            std::process::exit(1);
        }
    };
    match action {
        GatewaySecretAction::Set { name } => {
            use std::io::Read;
            let mut value = String::new();
            if std::io::stdin().read_to_string(&mut value).is_err() {
                crate::ui::error("failed to read secret value from stdin");
                std::process::exit(1);
            }
            let value = value.trim();
            if value.is_empty() {
                crate::ui::error("secret value must not be empty");
                std::process::exit(1);
            }
            match crate::gateway_secrets::set_secret(&conn, &name, value) {
                Ok(()) => println!("stored secret '{name}'"),
                Err(e) => {
                    crate::ui::error(&format!("failed to store secret: {e}"));
                    std::process::exit(1);
                }
            }
        }
        GatewaySecretAction::List => match crate::gateway_secrets::list_secrets(&conn) {
            Ok(names) if names.is_empty() => println!("no secrets stored"),
            Ok(names) => {
                for n in names {
                    println!("{n}");
                }
            }
            Err(e) => {
                crate::ui::error(&format!("failed to list secrets: {e}"));
                std::process::exit(1);
            }
        },
        GatewaySecretAction::Remove { name } => {
            match crate::gateway_secrets::remove_secret(&conn, &name) {
                Ok(true) => println!("removed secret '{name}'"),
                Ok(false) => {
                    crate::ui::error(&format!("no secret named '{name}'"));
                    std::process::exit(1);
                }
                Err(e) => {
                    crate::ui::error(&format!("failed to remove secret: {e}"));
                    std::process::exit(1);
                }
            }
        }
    }
}
