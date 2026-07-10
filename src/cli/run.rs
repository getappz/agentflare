use clap::Args;

/// Launch an agent through mise (so all mise-managed tools are on PATH for the
/// session and anything it spawns) with wrangler-style `.dev.vars` env vars
/// injected. Example: `agentflare run claude-code --env staging`.
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
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl RunArgs {
    pub fn run(self) {
        crate::agents::cli_run(
            &self.agent,
            self.env.as_deref(),
            self.model.as_deref(),
            self.mode.as_deref(),
            &self.args,
        );
    }
}
