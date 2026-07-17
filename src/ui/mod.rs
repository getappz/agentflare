//! Terminal UI for the CLI, built on the `cliclack` crate.
//!
//! Every interactive prompt and human-facing status line routes through this
//! module instead of hand-rolled `print!`/`println!` + `stdin().read_line()`.
//! Centralizing here fixes a bug class that hand-rolled prompts kept
//! reintroducing:
//!   * unflushed prompts — `print!` without a flush left the question invisible
//!     while `read_line` blocked, so a command looked hung when it was actually
//!     waiting for a keypress nobody could see.
//!   * blocking on a non-tty — piped/redirected stdin never delivers a line, so
//!     `read_line` blocked forever with no way out.
//!
//! Split by concern: [`log`] (status output), [`prompt`] (interactive input),
//! [`spinner`] (long-running work). Each helper checks [`interactive`] first and
//! falls back to plain text with a safe default off a terminal, so headless
//! runs (CI, hooks, pipes) stay readable and never hang.

mod log;
mod prompt;
mod spinner;

pub use log::{error, info, intro, outro, skip, step, success, warning};
pub use prompt::{confirm, select};
pub use spinner::with_spinner;

use std::io::IsTerminal;

/// Interactive UI is safe only when both stdin and stdout are real terminals,
/// `CI` is unset, and the caller hasn't opted out via `AGENTFLARE_NO_INTERACTIVE`.
/// TTY checks alone miss pseudo-TTY CI jobs and terminal-launched hooks, so both
/// env vars take priority over the TTY check.
pub fn interactive() -> bool {
    if std::env::var_os("CI").is_some() || std::env::var_os("AGENTFLARE_NO_INTERACTIVE").is_some() {
        return false;
    }
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}
