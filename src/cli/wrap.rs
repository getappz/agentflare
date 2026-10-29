use clap::{Args, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Copy, ValueEnum)]
pub enum WrappedAgent {
    #[value(name = "claude", alias = "claude-code")]
    Claude,
}

#[derive(Args)]
pub struct WrapArgs {
    /// Agent whose shell command should be wrapped (claude-code is an alias).
    #[arg(value_enum)]
    pub agent: WrappedAgent,
    /// Target shell; defaults to the current shell.
    #[arg(long)]
    pub shell: Option<String>,
    /// Shell profile to edit; defaults to the shell's normal profile.
    #[arg(long)]
    pub profile: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub launch: bool,
    #[arg(last = true, hide = true)]
    pub args: Vec<std::ffi::OsString>,
}

impl WrapArgs {
    pub fn run(self, remove: bool) {
        let result = if self.launch && !remove && self.shell.is_none() && self.profile.is_none() {
            crate::wrap::launch(&self.args)
        } else if self.launch || !self.args.is_empty() {
            Err("Native Claude arguments are accepted through the installed claude wrapper".into())
        } else {
            let _ = self.agent;
            crate::wrap::setup(self.shell.as_deref(), self.profile.as_deref(), remove).map(|()| 0)
        };
        match result {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                crate::ui::error(&error);
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn both_claude_names_parse_and_native_arguments_survive() {
        for agent in ["claude", "claude-code"] {
            let cli = Cli::try_parse_from([
                "agentflare",
                "wrap",
                agent,
                "--launch",
                "--",
                "--with-proxy",
                "--resume",
                "a b",
            ])
            .unwrap();
            let Some(Commands::Wrap(args)) = cli.command else {
                panic!("wrap expected")
            };
            assert_eq!(args.args, ["--with-proxy", "--resume", "a b"]);
            assert!(args.launch);
        }
        assert!(Cli::try_parse_from(["agentflare", "wrap", "codex"]).is_err());
    }
}
