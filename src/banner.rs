//! Shared branding banner — single source of truth (mirrors `assets/banner.txt`).
//!
//! Installers and the README carry their own copy of this text; keep them in
//! sync. Unlike lean-ctx (whose logo lived only in the README and drifted from
//! the binary), agentflare centralizes the banner here so the CLI, installers,
//! and docs cannot diverge.

pub const BANNER: &str = include_str!("../assets/banner.txt");

use crate::ui::interactive;

/// Print the branding banner to stdout.
///
/// Color is suppressed when the session is non-interactive (per
/// [`crate::ui::interactive`]) or `NO_COLOR` is set, so piped/CI output stays
/// plain. Divider lines render dim-cyan, the wordmark line bright-magenta — but
/// only when emitting color is actually safe.
pub fn print_banner() {
    print!("{}", colorize(BANNER));
}

fn colorize(s: &str) -> String {
    if !interactive() || std::env::var_os("NO_COLOR").is_some() {
        return s.to_string();
    }
    s.lines()
        .map(|line| {
            if line.starts_with('━') {
                format!("\x1b[2;36m{line}\x1b[0m")
            } else {
                format!("\x1b[1;35m{line}\x1b[0m")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
