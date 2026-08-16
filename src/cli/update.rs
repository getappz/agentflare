use clap::Args;

/// Update the agentflare binary to the latest, or a specific, version.
#[derive(Args)]
pub struct UpdateArgs {
    pub version: Option<String>,
    #[arg(long)]
    pub check: bool,
    #[arg(long)]
    pub quiet: bool,
}

impl UpdateArgs {
    pub fn run(self) {
        crate::update::run(self.version, self.check, self.quiet);
    }
}
