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
}

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
