use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum VaultAction {
    /// Unlock the vault (create if missing + cache passphrase).
    Unlock {
        /// Read passphrase from stdin instead of prompting.
        #[arg(long)]
        stdin: bool,
    },
    /// Lock the vault (clear cached passphrase and session DEK).
    Lock,
    /// Print vault env vars for the current project.
    Env,
    /// Set a secret's value, read from stdin (never as a CLI argument, so it
    /// never lands in shell history).
    Set { name: String },
    /// List the names of stored secrets (never their values).
    List,
    /// Remove a stored secret.
    Remove { name: String },
}

/// Manage the local secret vault (passphrase-locked store for project secrets).
#[derive(Args)]
pub struct VaultArgs {
    #[command(subcommand)]
    pub action: VaultAction,
}

impl VaultArgs {
    pub fn run(self) {
        match self.action {
            VaultAction::Unlock { stdin } => run_unlock(stdin),
            VaultAction::Lock => run_lock(),
            VaultAction::Env => run_env(),
            VaultAction::Set { name } => run_set(&name),
            VaultAction::List => run_list(),
            VaultAction::Remove { name } => run_remove(&name),
        }
    }
}

fn run_unlock(stdin: bool) {
    let passphrase = if stdin {
        use std::io::Read;
        let mut pw = String::new();
        if std::io::stdin().read_to_string(&mut pw).is_err() {
            crate::ui::error("failed to read passphrase from stdin");
            std::process::exit(1);
        }
        // Strip only the trailing line ending (how a piped passphrase
        // usually arrives), not surrounding whitespace that could be part
        // of the intended passphrase.
        pw.trim_end_matches(['\r', '\n']).to_string()
    } else {
        rpassword::prompt_password("vault passphrase: ").unwrap_or_default()
    };
    if passphrase.is_empty() {
        crate::ui::error("passphrase must not be empty");
        std::process::exit(1);
    }
    match crate::vault::unlock(&passphrase) {
        Ok(()) => println!("vault unlocked"),
        Err(e) => {
            crate::ui::error(&format!("failed to unlock vault: {e}"));
            std::process::exit(1);
        }
    }
}

fn run_lock() {
    match crate::vault::lock() {
        Ok(()) => println!("vault locked"),
        Err(e) => {
            crate::ui::error(&format!("failed to lock vault: {e}"));
            std::process::exit(1);
        }
    }
}

fn run_env() {
    let cwd = std::env::current_dir().unwrap_or_default();
    let env = crate::vault::vault_env(&cwd);
    if env.is_empty() {
        return;
    }
    for (k, v) in &env {
        println!("{k}={v}");
    }
}

/// Store a secret's value, read from stdin so it never lands in shell
/// history. Also reachable via the deprecated `agentflare gateway secret
/// set` alias.
pub(crate) fn run_set(name: &str) {
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
    match crate::vault::set_secret(name, value) {
        Ok(()) => println!("stored secret '{name}'"),
        Err(e) => {
            crate::ui::error(&format!("failed to store secret: {e}"));
            std::process::exit(1);
        }
    }
}

/// List stored secret names (never their values). Also reachable via the
/// deprecated `agentflare gateway secret list` alias.
pub(crate) fn run_list() {
    match crate::vault::list_secrets() {
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
    }
}

/// Remove a stored secret. Also reachable via the deprecated `agentflare
/// gateway secret remove` alias.
pub(crate) fn run_remove(name: &str) {
    match crate::vault::remove_secret(name) {
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
