use clap::Args;

/// Launch an agent through mise (so all mise-managed tools are on PATH for the
/// session and anything it spawns) with wrangler-style `.dev.vars` env vars
/// injected. Example: `agentflare run claude-code --env staging`.
///
/// With `--print`, run the agent non-interactively on the given prompt and
/// write its reply to stdout (for scripts, cron, and orchestrators).
#[derive(Args)]
pub struct RunArgs {
    /// Agent to launch (e.g. claude-code).
    pub agent: String,
    /// Env stage: load `.dev.vars.<stage>` instead of `.dev.vars` (replaces it).
    #[arg(long)]
    pub env: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub mode: Option<String>,
    /// Run non-interactively on this prompt and print the reply to stdout.
    #[arg(long, value_name = "PROMPT")]
    pub print: Option<String>,
    /// Timeout (seconds) for `--print` before the agent is killed.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl RunArgs {
    /// Which interactive-only flags were set alongside `--print`. `--print`
    /// runs a separate headless path (`cli_run_headless`) that never sees
    /// `model`/`mode`/`env`/`args` — silently proceeding would drop them on
    /// the floor, so callers must reject the combination instead.
    fn print_flag_conflicts(&self) -> Vec<&'static str> {
        if self.print.is_none() {
            return Vec::new();
        }
        let mut conflicts = Vec::new();
        if self.model.is_some() {
            conflicts.push("--model");
        }
        if self.mode.is_some() {
            conflicts.push("--mode");
        }
        if self.env.is_some() {
            conflicts.push("--env");
        }
        if !self.args.is_empty() {
            conflicts.push("trailing args");
        }
        conflicts
    }

    pub fn run(self) {
        if let Some(prompt) = self.print.as_deref() {
            let conflicts = self.print_flag_conflicts();
            if !conflicts.is_empty() {
                eprintln!(
                    "error: --print does not support {} yet (interactive `run` only) — drop it or omit --print",
                    conflicts.join(", ")
                );
                std::process::exit(1);
            }
            let code = crate::agents::cli_run_headless(
                &self.agent,
                prompt,
                std::time::Duration::from_secs(self.timeout),
            );
            std::process::exit(code);
        }
        crate::agents::cli_run(
            &self.agent,
            self.env.as_deref(),
            self.model.as_deref(),
            self.mode.as_deref(),
            &self.args,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args(print: Option<&str>) -> RunArgs {
        RunArgs {
            agent: "claude-code".to_string(),
            env: None,
            model: None,
            mode: None,
            print: print.map(str::to_string),
            timeout: 120,
            args: Vec::new(),
        }
    }

    #[test]
    fn no_conflicts_without_print() {
        let mut args = base_args(None);
        args.model = Some("opus".to_string());
        args.mode = Some("plan".to_string());
        args.env = Some("staging".to_string());
        args.args = vec!["--foo".to_string()];
        assert_eq!(args.print_flag_conflicts(), Vec::<&str>::new());
    }

    #[test]
    fn no_conflicts_with_print_alone() {
        let args = base_args(Some("hello"));
        assert_eq!(args.print_flag_conflicts(), Vec::<&str>::new());
    }

    #[test]
    fn print_with_model_conflicts() {
        let mut args = base_args(Some("hello"));
        args.model = Some("opus".to_string());
        assert_eq!(args.print_flag_conflicts(), vec!["--model"]);
    }

    #[test]
    fn print_with_mode_conflicts() {
        let mut args = base_args(Some("hello"));
        args.mode = Some("plan".to_string());
        assert_eq!(args.print_flag_conflicts(), vec!["--mode"]);
    }

    #[test]
    fn print_with_env_conflicts() {
        let mut args = base_args(Some("hello"));
        args.env = Some("staging".to_string());
        assert_eq!(args.print_flag_conflicts(), vec!["--env"]);
    }

    #[test]
    fn print_with_trailing_args_conflicts() {
        let mut args = base_args(Some("hello"));
        args.args = vec!["extra".to_string()];
        assert_eq!(args.print_flag_conflicts(), vec!["trailing args"]);
    }

    #[test]
    fn print_with_all_conflicts_names_all_flags() {
        let mut args = base_args(Some("hello"));
        args.model = Some("opus".to_string());
        args.mode = Some("plan".to_string());
        args.env = Some("staging".to_string());
        args.args = vec!["extra".to_string()];
        assert_eq!(
            args.print_flag_conflicts(),
            vec!["--model", "--mode", "--env", "trailing args"]
        );
    }
}
