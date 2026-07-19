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
