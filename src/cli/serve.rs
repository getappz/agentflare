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
    /// Acknowledge exposing all PM/cost/webhook data with no authentication.
    /// Required whenever --host is not 127.0.0.1/localhost/::1.
    #[arg(long)]
    pub yes_expose: bool,
    /// Internal: marks this invocation as the daemon-managed foreground
    /// process (spawned by `agentflare daemon start` or the installed
    /// systemd/launchd unit). Not meant for direct use; the singleton check
    /// and pid registration below apply either way.
    #[arg(long = "_foreground-daemon", hide = true)]
    pub foreground_daemon: bool,
}

impl ServeArgs {
    pub fn run(self) {
        if let Some(pid) = crate::daemon::is_daemon_running() {
            eprintln!("agentflare dashboard is already running (pid {pid}).");
            eprintln!(
                "stop it first with `agentflare daemon stop`, or use the running instance instead of starting another."
            );
            std::process::exit(1);
        }
        if let Err(e) = crate::daemon::write_pid_file() {
            eprintln!(
                "warning: failed to record daemon pid ({e}); `agentflare daemon status`/`stop` won't see this instance."
            );
        }
        crate::dashboard::serve(&self.host, self.port, self.open, self.yes_expose);
    }
}
