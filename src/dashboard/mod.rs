mod artifacts;
mod data;
mod orphan_reconcile;
mod server;

pub fn serve(host: &str, port: u16, open: bool, yes_expose: bool) {
    let runtime = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    runtime.block_on(server::run(host, port, open, yes_expose));
}

/// Best-effort: open a URL in the default browser. Never panics.
pub fn open_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}
