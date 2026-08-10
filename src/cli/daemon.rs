use clap::{Args, Subcommand};

#[derive(Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub command: DaemonSubcommand,
}

#[derive(Subcommand)]
pub enum DaemonSubcommand {
    Start,
    Stop,
    Restart,
    Status,
    Enable,
    Disable,
    /// Print the current daemon session's stdout+stderr (bridge activity,
    /// dashboard startup, etc). Truncated fresh on every start/restart.
    Logs {
        /// Keep printing new lines as the daemon writes them.
        #[arg(short, long)]
        follow: bool,
    },
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
        }
    }
}

fn cmd_start() {
    match crate::daemon::start_daemon() {
        Ok(pid) => println!("daemon started (pid {pid})"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_stop() {
    match crate::daemon::stop_daemon() {
        Ok(()) => println!("daemon stopped"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_restart() {
    let _ = crate::daemon::stop_daemon();
    match crate::daemon::start_daemon() {
        Ok(pid) => println!("daemon restarted (pid {pid})"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_status() {
    match crate::daemon::is_daemon_running() {
        Some(pid) => println!("daemon running (pid {pid})"),
        None => {
            println!("daemon not running");
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
            eprintln!("error: {}: {e}", path.display());
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
        Ok(()) => println!("autostart enabled"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_disable() {
    match crate::daemon_autostart::uninstall() {
        Ok(()) => println!("autostart disabled"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
