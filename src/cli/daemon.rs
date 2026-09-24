use clap::{Args, Subcommand};

#[derive(Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub command: DaemonSubcommand,
}

#[derive(Subcommand)]
pub enum DaemonSubcommand {
    /// Start the daemon (dashboard, bridge, watchers) if not already running.
    Start,
    /// Stop the running daemon.
    Stop,
    /// Stop and restart the daemon.
    Restart,
    /// Report whether the daemon is running, and its pid.
    Status,
    /// Install a systemd/launchd unit so the daemon starts automatically.
    Enable,
    /// Remove the autostart unit installed by `enable`.
    Disable,
    /// Print the current daemon session's stdout+stderr (bridge activity,
    /// dashboard startup, etc). Truncated fresh on every start/restart.
    Logs {
        /// Keep printing new lines as the daemon writes them.
        #[arg(short, long)]
        follow: bool,
    },
    /// Internal: run the workflow-store boot smoke test standalone (save,
    /// delete, confirm gone) and exit 0/1. `dashboard::server::run` runs the
    /// same check in-process on every daemon start; this hidden verb lets
    /// `dev_install::run` (item #164) run it against a freshly built binary
    /// right after a binary swap -- the exact trigger (item #576) that let a
    /// broken `delete_state` go unnoticed for ~33h. Not meant to be run by
    /// hand.
    #[command(hide = true, name = "workflow-store-smoke-test")]
    WorkflowStoreSmokeTest,
    /// Host resource dispatch gate controls (item #643). A gate stuck
    /// paused (e.g. a stray `AGENTFLARE_DISPATCH_GATE_MODE=off` baked into
    /// the daemon's environment) had no reset path short of env-var
    /// archaeology, and `daemon restart` alone doesn't clear it -- restart
    /// just re-reads the same stuck env var.
    Gate {
        #[command(subcommand)]
        command: GateSubcommand,
    },
}

#[derive(Subcommand)]
pub enum GateSubcommand {
    /// Show the resource gate's current policy and pause reason.
    Status(GateArgs),
    /// Force-unpause a gate stuck on `AGENTFLARE_DISPATCH_GATE_MODE=off`.
    /// Does not override a gate paused for genuine CPU pressure -- that
    /// reason self-clears once CPU drops, and isn't what this targets.
    Reset(GateArgs),
}

#[derive(Args)]
pub struct GateArgs {
    /// Dashboard host to reach. The gate's state lives in the running
    /// daemon process, so this talks to it over HTTP rather than reading
    /// local state -- the daemon must already be running.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Dashboard port, matching `agentflare serve`'s own default.
    #[arg(long, default_value = "35273")]
    pub port: u16,
}

impl GateSubcommand {
    fn run(self) {
        match self {
            GateSubcommand::Status(args) => cmd_gate_status(args),
            GateSubcommand::Reset(args) => cmd_gate_reset(args),
        }
    }
}

impl DaemonArgs {
    pub fn run(self) {
        match self.command {
            DaemonSubcommand::Start => cmd_start(),
            DaemonSubcommand::Stop => cmd_stop(),
            DaemonSubcommand::Restart => cmd_restart(),
            DaemonSubcommand::Status => cmd_status(),
            DaemonSubcommand::Enable => cmd_enable(),
            DaemonSubcommand::Disable => cmd_disable(),
            DaemonSubcommand::Logs { follow } => cmd_logs(follow),
            DaemonSubcommand::WorkflowStoreSmokeTest => cmd_workflow_store_smoke_test(),
            DaemonSubcommand::Gate { command } => command.run(),
        }
    }
}

fn cmd_start() {
    match crate::daemon::start_daemon() {
        Ok(pid) => crate::ui::success(&format!("daemon started (pid {pid})")),
        Err(e) => {
            crate::ui::error(&e.to_string());
            std::process::exit(1);
        }
    }
}

fn cmd_stop() {
    match crate::daemon::stop_daemon() {
        Ok(()) => crate::ui::success("daemon stopped"),
        Err(e) => {
            crate::ui::error(&e.to_string());
            std::process::exit(1);
        }
    }
}

fn cmd_restart() {
    let _ = crate::daemon::stop_daemon();
    match crate::daemon::start_daemon() {
        Ok(pid) => crate::ui::success(&format!("daemon restarted (pid {pid})")),
        Err(e) => {
            crate::ui::error(&e.to_string());
            std::process::exit(1);
        }
    }
}

fn cmd_status() {
    match crate::daemon::is_daemon_running() {
        Some(pid) => crate::ui::info(&format!("daemon running (pid {pid})")),
        None => {
            crate::ui::info("daemon not running");
            std::process::exit(1);
        }
    }
}

fn cmd_logs(follow: bool) {
    use std::io::{Read, Write};
    let path = crate::daemon::daemon_log_path();
    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            crate::ui::error(&format!("{}: {e}", path.display()));
            std::process::exit(1);
        }
    };
    let mut buf = String::new();
    let _ = file.read_to_string(&mut buf);
    print!("{buf}");
    let _ = std::io::stdout().flush();
    if !follow {
        return;
    }
    // Simplest portable tail -f: re-read from the current position on the
    // same handle, which reflects append-mode writes made by a different
    // process to the same file. Not robust to the daemon restarting mid-tail
    // (a fresh log truncates the same path) -- good enough for `-f` used
    // interactively, same tradeoff `agentflare work`'s own log tailing makes.
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let mut chunk = String::new();
        if file.read_to_string(&mut chunk).is_ok() && !chunk.is_empty() {
            print!("{chunk}");
            let _ = std::io::stdout().flush();
        }
    }
}

fn cmd_enable() {
    match crate::daemon_autostart::install() {
        Ok(()) => crate::ui::success("autostart enabled"),
        Err(e) => {
            crate::ui::error(&e.to_string());
            std::process::exit(1);
        }
    }
}

fn cmd_disable() {
    match crate::daemon_autostart::uninstall() {
        Ok(()) => crate::ui::success("autostart disabled"),
        Err(e) => {
            crate::ui::error(&e.to_string());
            std::process::exit(1);
        }
    }
}

fn cmd_gate_status(args: GateArgs) {
    match ureq::get(&format!("http://{}:{}/api/gate", args.host, args.port)).call() {
        Ok(resp) => print_gate_response(resp),
        Err(e) => gate_request_error(&e),
    }
}

fn cmd_gate_reset(args: GateArgs) {
    match ureq::post(&format!(
        "http://{}:{}/api/gate/reset",
        args.host, args.port
    ))
    .call()
    {
        Ok(resp) => {
            print_gate_response(resp);
            crate::ui::success(
                "gate force-resumed; a stuck `AGENTFLARE_DISPATCH_GATE_MODE=off` no longer blocks dispatch (genuine CPU pressure still will)",
            );
        }
        Err(e) => gate_request_error(&e),
    }
}

fn print_gate_response(resp: ureq::Response) {
    match resp.into_string() {
        Ok(body) => crate::ui::info(&body),
        Err(e) => {
            crate::ui::error(&format!("failed to read dashboard response: {e}"));
            std::process::exit(1);
        }
    }
}

fn gate_request_error(e: &ureq::Error) {
    crate::ui::error(&format!(
        "failed to reach dashboard ({e}); is the daemon running? (`agentflare daemon start` / `agentflare daemon status`)"
    ));
    std::process::exit(1);
}

fn cmd_workflow_store_smoke_test() {
    let store = crate::work_item_pipeline::engine().state_store();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(flare_workflow::smoke_test(store));
    match result {
        Ok(()) => crate::ui::success("workflow store smoke test passed"),
        Err(e) => {
            crate::ui::error(&format!("workflow store smoke test failed: {e}"));
            std::process::exit(1);
        }
    }
}
