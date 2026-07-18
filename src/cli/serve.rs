use clap::Args;

/// Serve the read-only agentflare dashboard.
#[derive(Args)]
pub struct ServeArgs {
    /// TCP port. Default 35273 ("FLARE" on a phone keypad); 0 = auto-assign.
    #[arg(long, default_value = "35273")]
    pub port: u16,
    /// Interface to bind ("0.0.0.0" shares with your LAN).
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Open the dashboard in your browser once the server is up.
    #[arg(long)]
    pub open: bool,
}

impl ServeArgs {
    pub fn run(self) {
        crate::dashboard::serve(&self.host, self.port, self.open);
    }
}
