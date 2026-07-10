use clap::Args;

/// Serve live-shareable artifact pages from AI agent sessions.
#[derive(Args)]
pub struct ArtifactsArgs {
    /// TCP port (0 = auto).
    #[arg(long, default_value = "0")]
    pub port: u16,
    /// Storage directory (default: ~/.agentflare/artifacts).
    #[arg(long)]
    pub dir: Option<std::path::PathBuf>,
}

impl ArtifactsArgs {
    pub fn run(self) {
        crate::artifacts::serve(self.port, self.dir);
    }
}
