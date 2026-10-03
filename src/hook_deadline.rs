//! Hard wall-clock budgets for agent lifecycle hooks. Claude Code sets a
//! per-hook `timeout` in `settings.json`, but on Windows the child process
//! can keep running long after the host marks the hook timed out (session
//! jsonl `durationMs` >> `timeoutMs`). We self-terminate slightly under the
//! wired timeout so stray work (especially unbounded `git` spawns) cannot
//! wedge the agent loop for tens of seconds.

use std::time::Duration;

/// Hook kinds wired by [`init::claude_hook_specs`]; budgets must stay below
/// the matching `timeout` field there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    PreToolUse,
    PromptSubmit,
    SessionStart,
    PostToolFailure,
    PostToolUse,
    SessionEnd,
    Stop,
    PreCompact,
}

impl Event {
    #[must_use]
    pub(crate) fn wall_clock(self) -> Duration {
        match self {
            Self::PreToolUse | Self::PromptSubmit => Duration::from_millis(4_500),
            Self::SessionStart => Duration::from_millis(9_500),
            Self::PostToolFailure
            | Self::PostToolUse
            | Self::SessionEnd
            | Self::Stop
            | Self::PreCompact => Duration::from_millis(4_500),
        }
    }
}

pub(crate) struct Guard {
    _thread: std::thread::JoinHandle<()>,
}

/// Spawns a sleeper that [`std::process::exit`]s with code 0 once the budget
/// elapses. Hooks fail open: no JSON on stdout is valid for most events once
/// the host has already moved on.
pub(crate) fn install(event: Event) -> Guard {
    let budget = event.wall_clock();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(budget);
        eprintln!("[agentflare] hook: exceeded wall-clock budget ({budget:?}) — exiting");
        std::process::exit(0);
    });
    Guard { _thread: thread }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_budgets_sit_under_claude_code_hook_timeouts() {
        assert!(Event::PreToolUse.wall_clock() < Duration::from_secs(5));
        assert!(Event::PromptSubmit.wall_clock() < Duration::from_secs(5));
        assert!(Event::SessionStart.wall_clock() < Duration::from_secs(10));
        assert!(Event::PostToolUse.wall_clock() < Duration::from_secs(5));
    }
}
