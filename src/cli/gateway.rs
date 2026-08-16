use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum GatewayAction {
    /// Deprecated: use `agentflare vault` instead.
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

/// Deprecated: secret management moved to `agentflare vault`.
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

/// Deprecated: secret management moved to `agentflare vault set|list|remove`
/// so there's a single canonical place to manage vault secrets instead of
/// two commands fronting the same store. Kept as a forwarding alias so
/// existing scripts keep working.
fn run_secret(action: GatewaySecretAction) {
    crate::ui::warning(
        "agentflare gateway secret is deprecated, use `agentflare vault set|list|remove` instead",
    );
    match action {
        GatewaySecretAction::Set { name } => crate::cli::vault::run_set(&name),
        GatewaySecretAction::List => crate::cli::vault::run_list(),
        GatewaySecretAction::Remove { name } => crate::cli::vault::run_remove(&name),
    }
}
