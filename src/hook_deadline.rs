//! Hard wall-clock budgets for agent lifecycle hooks. Claude Code sets a
//! per-hook `timeout` in `settings.json`, but on Windows the child process
//! can keep running long after the host marks the hook timed out (session
//! jsonl `durationMs` >> `timeoutMs`). We self-terminate slightly under the
//! wired timeout so stray work (especially unbounded `git` spawns) cannot
//! wedge the agent loop for tens of seconds.

use std::time::Duration;

/// Headroom reserved for agent detection and teardown inside the host timeout.
const BUDGET_HEADROOM: Duration = Duration::from_millis(500);

/// Hook kinds wired by [`init::claude_hook_specs`]; only a subset install a
/// self-deadline (see [`Event::wall_clock`]).
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
    fn claude_settings_event(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PromptSubmit => "UserPromptSubmit",
            Self::SessionStart => "SessionStart",
            Self::PostToolFailure => "PostToolUseFailure",
            Self::PostToolUse => "PostToolUse",
            Self::SessionEnd => "SessionEnd",
            Self::Stop => "Stop",
            Self::PreCompact => "PreCompact",
        }
    }

    /// Wall-clock budget for hooks that install a self-deadline. Derived from
    /// [`init::claude_hook_specs`] (`timeout` minus [`BUDGET_HEADROOM`]). Hooks
    /// that can legitimately run longer (PostToolUse verification gate, Stop
    /// messaging, etc.) return `None` even when Claude Code carries a host
    /// timeout — we must not kill that work at ~4.5 s.
    #[must_use]
    pub(crate) fn wall_clock(self) -> Option<Duration> {
        let specs = crate::init::claude_hook_specs("agentflare");
        let event_name = self.claude_settings_event();
        let spec = specs
            .iter()
            .find(|s| s.event == event_name)
            .expect("hook_deadline Event must match a claude_hook_specs entry");
        if !spec.install_self_deadline {
            return None;
        }
        let host = Duration::from_secs(spec.timeout);
        Some(host.saturating_sub(BUDGET_HEADROOM))
    }
}

pub(crate) struct Guard {
    _thread: std::thread::JoinHandle<()>,
}

/// Spawns a sleeper that [`std::process::exit`]s with code 0 once the budget
/// elapses. Hooks fail open: no JSON on stdout is valid for most events once
/// the host has already moved on. No-op when [`Event::wall_clock`] is `None`.
pub(crate) fn install(event: Event) -> Option<Guard> {
    let budget = event.wall_clock()?;
    let thread = std::thread::spawn(move || {
        std::thread::sleep(budget);
        eprintln!("[agentflare] hook: exceeded wall-clock budget ({budget:?}) — exiting");
        std::process::exit(0);
    });
    Some(Guard { _thread: thread })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_budgets_sit_under_claude_code_hook_timeouts() {
        let specs = crate::init::claude_hook_specs("agentflare");
        for spec in &specs {
            let event = match spec.event {
                "PreToolUse" => Event::PreToolUse,
                "UserPromptSubmit" => Event::PromptSubmit,
                "SessionStart" => Event::SessionStart,
                "PostToolUse" => Event::PostToolUse,
                "Stop" => Event::Stop,
                _ => continue,
            };
            if spec.install_self_deadline {
                let budget = event
                    .wall_clock()
                    .expect("spec marks install_self_deadline");
                assert!(budget < Duration::from_secs(spec.timeout));
            } else {
                assert!(
                    event.wall_clock().is_none(),
                    "{} must not install a self-deadline",
                    spec.event
                );
            }
        }
    }
}
