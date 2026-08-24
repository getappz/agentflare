//! Console-window-safe child-process spawning.
//!
//! On Windows, spawning a console-subsystem binary from a parent that has no
//! inherited console (the daemon, an MCP server started by a GUI IDE, a hook
//! runner) makes the OS allocate a brand-new console for the child --
//! rendered to the user as a terminal window flashing over whatever they are
//! doing. `CREATE_NO_WINDOW` suppresses that allocation and is a no-op on
//! every other platform.
//!
//! Every captured/background spawn must go through [`command`] (or apply
//! [`no_window`] to a builder that was already constructed). Fixing spawn
//! sites piecemeal regresses: earendil-works/pi fixed this exact bug twice
//! (#4699, #5113) before their post-mortem on the third occurrence (#5529)
//! concluded the fix belongs in ONE central constructor every spawn passes
//! through, not at each call site.
//!
//! Deliberately NOT used for interactive, user-facing spawns: agent CLIs
//! launched into the caller's terminal (`launch_agent`), `agentflare auth
//! login`'s child, browser open. Script-shim agents also need a real console
//! (see `agent_launch::run_headless`'s `-WindowStyle Hidden` launcher) --
//! `CREATE_NO_WINDOW` hangs those.

use std::ffi::OsStr;
use std::process::Command;

/// `Command::new(program)` plus `CREATE_NO_WINDOW` on Windows: the child never
/// allocates a console window of its own. Only fresh console *allocation* is
/// suppressed -- piped handles behave identically, and handles inherited from
/// a parent that does own a terminal still write through to it, so captured
/// `.output()` spawns and progress-printing installs are unaffected.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    no_window(&mut cmd);
    cmd
}

/// The same suppression applied to an existing builder -- for call sites that
/// construct the `Command` incrementally (env/cwd wiring spread across
/// branches) or wrap `std::process::Command`.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: the child gets no console of its own. Not the
        // same as DETACHED_PROCESS -- we only want the window gone, normal
        // lifetime/job semantics kept.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_spawn_runs_and_captures_output() {
        #[cfg(windows)]
        let output = command("cmd").args(["/C", "echo ok"]).output().unwrap();
        #[cfg(not(windows))]
        let output = command("sh").args(["-c", "echo ok"]).output().unwrap();

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
    }

    #[test]
    fn no_window_retrofits_an_existing_builder() {
        #[cfg(windows)]
        let mut cmd = Command::new("cmd");
        #[cfg(not(windows))]
        let mut cmd = Command::new("sh");
        no_window(&mut cmd);

        #[cfg(windows)]
        cmd.args(["/C", "exit 0"]);
        #[cfg(not(windows))]
        cmd.args(["-c", "exit 0"]);

        assert!(cmd.status().unwrap().success());
    }
}
