use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Manage `~/.agentflare/config.toml` settings that aren't repo-scoped (see
/// `agentflare github-bridge` for the repo-scoped `[bridge]` overrides).
#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Set a config value.
    Set { key: ConfigKey, value: String },
    /// Print a config value's resolved (post-fallback) value.
    Get { key: ConfigKey },
    /// Remove a config value, reverting to its fallback.
    Unset { key: ConfigKey },
}

#[derive(Copy, Clone, ValueEnum)]
pub enum ConfigKey {
    /// Friendly name for this workstation, used in `beacon:<name>` GitHub
    /// labels and PR footers in place of the hashed instance id.
    #[value(name = "machine-name")]
    MachineName,
}

impl ConfigArgs {
    pub fn run(self) {
        match self.action {
            ConfigAction::Set { key, value } => cmd_set(key, &value),
            ConfigAction::Get { key } => cmd_get(key),
            ConfigAction::Unset { key } => cmd_unset(key),
        }
    }
}

fn set_result(key: ConfigKey, value: &str) -> Result<PathBuf, String> {
    match key {
        ConfigKey::MachineName => crate::github::bridge::config::write_machine_name(value),
    }
}

fn unset_result(key: ConfigKey) -> Result<PathBuf, String> {
    match key {
        ConfigKey::MachineName => crate::github::bridge::config::clear_machine_name(),
    }
}

fn get_value(key: ConfigKey) -> String {
    match key {
        ConfigKey::MachineName => crate::github::bridge::config::machine_label(),
    }
}

fn cmd_set(key: ConfigKey, value: &str) {
    match set_result(key, value) {
        Ok(path) => crate::ui::success(&format!("wrote {}", path.display())),
        Err(e) => {
            crate::ui::error(&e.to_string());
            std::process::exit(1);
        }
    }
}

fn cmd_get(key: ConfigKey) {
    println!("{}", get_value(key));
}

fn cmd_unset(key: ConfigKey) {
    match unset_result(key) {
        Ok(path) => crate::ui::success(&format!("cleared in {}", path.display())),
        Err(e) => {
            crate::ui::error(&e.to_string());
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_get_machine_name_round_trips() {
        crate::paths::test_support::with_temp_home(|| {
            assert_eq!(
                get_value(ConfigKey::MachineName),
                crate::github::bridge::config::stable_instance_id(),
                "unset should fall back to the stable instance id"
            );
            set_result(ConfigKey::MachineName, "kumar-laptop").unwrap();
            assert_eq!(get_value(ConfigKey::MachineName), "kumar-laptop");
        });
    }

    #[test]
    fn unset_machine_name_falls_back_to_stable_instance_id() {
        crate::paths::test_support::with_temp_home(|| {
            set_result(ConfigKey::MachineName, "kumar-laptop").unwrap();
            unset_result(ConfigKey::MachineName).unwrap();
            assert_eq!(
                get_value(ConfigKey::MachineName),
                crate::github::bridge::config::stable_instance_id()
            );
        });
    }

    #[test]
    fn unset_machine_name_is_a_noop_when_never_set() {
        crate::paths::test_support::with_temp_home(|| {
            unset_result(ConfigKey::MachineName).unwrap();
        });
    }

    #[test]
    fn set_machine_name_rejects_whitespace_and_slash() {
        crate::paths::test_support::with_temp_home(|| {
            assert!(set_result(ConfigKey::MachineName, "kumar laptop").is_err());
            assert!(set_result(ConfigKey::MachineName, "kumar/laptop").is_err());
        });
    }

    #[test]
    fn config_key_parses_from_kebab_case_cli_value() {
        assert!(matches!(
            ConfigKey::from_str("machine-name", false),
            Ok(ConfigKey::MachineName)
        ));
    }
}
