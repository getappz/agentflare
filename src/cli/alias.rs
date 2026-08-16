use clap::Args;

/// Install a short shell alias for `agentflare` (default `af`, falling back
/// through `agf`/`afl`/`agentf` if taken) into your shell profile.
#[derive(Args)]
pub struct AliasArgs {
    /// Alias name to install instead of `af`.
    pub preferred: Option<String>,
    /// Overwrite an existing agentflare alias block instead of leaving it.
    #[arg(long)]
    pub force: bool,
    /// Print the shell snippet instead of writing it to the profile.
    #[arg(long)]
    pub print: bool,
    /// Skip the confirmation prompt.
    #[arg(long)]
    pub yes: bool,
    /// Shell to target instead of auto-detecting (e.g. "bash", "zsh", "fish").
    #[arg(long)]
    pub shell: Option<String>,
    /// Profile file to write to instead of the shell's default.
    #[arg(long)]
    pub profile: Option<String>,
    /// Print the result as JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

impl AliasArgs {
    pub fn run(self) {
        crate::alias::run(
            self.preferred,
            self.force,
            self.print,
            self.yes,
            self.shell,
            self.profile,
            self.json,
        );
    }
}
