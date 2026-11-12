//! How each agent's CLI is driven headless (prompt on stdin, output
//! captured): one [`HeadlessCli`] row per agent, so supporting a new agent
//! or flag touches one place. Flags are hand-verified against each CLI's
//! `--help`; an agent whose behaviour isn't verified has no row (or a `None`
//! field), and callers fall back to plain text rather than guess.

use crate::registry::Agent;

/// One agent's headless invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadlessCli {
    /// Subcommand/flags for non-interactive "print" mode (`claude -p`,
    /// `codex exec`). The prompt is piped on stdin, never an argument.
    pub print: &'static [&'static str],
    /// Unattended-mode flags for `agentflare work` (never `run --print`).
    /// Required: without them a permission prompt stalls the dispatch.
    pub autonomous: &'static [&'static str],
    /// Structured output, when its shape is verified.
    pub structured: Option<Structured>,
    /// Flag (or, for Codex, subcommand) placed before a session id to
    /// resume that session.
    pub resume: Option<&'static str>,
    /// Argv (id appended) that deletes a session, for agents whose service
    /// outlives the turn and could resume it after we killed the turn.
    /// `None` = nothing to clean up (or not verified).
    pub discard: Option<&'static [&'static str]>,
}

/// An agent's structured-output mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Structured {
    /// Flags that enable it. Streaming (one event per line) wherever the
    /// CLI supports it: a whole-reply `json` mode writes nothing until the
    /// run ends, so a long task trips the idle timeout (items #43, #183).
    pub args: &'static [&'static str],
    /// The shape the agent emits under `args`.
    pub format: ReplyFormat,
    /// The CLI's terminal `result` event is its last word, so the run may be
    /// ended shortly after it even if background tools still hold the
    /// process open. Verified for Claude Code only; elsewhere an early
    /// result-shaped line could cut off unfinished work as a "success".
    pub ends_at_result: bool,
}

/// Shape of a structured reply; selects the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyFormat {
    /// Claude Code's `result` object: the whole stdout (`json`) or the final
    /// line (`stream-json`) carries `result`, `session_id`,
    /// `total_cost_usd`, `usage`, ...
    ResultObject,
    /// Codex `exec --json` JSONL events (`thread.started`,
    /// `item.completed`, `turn.completed`, ...).
    CodexEvents,
    /// opencode `run --format json` JSONL events (`step_start`, `text`,
    /// `tool_use`, `step_finish` with tokens/cost), each carrying
    /// `sessionID`.
    OpencodeEvents,
}

/// The headless row for `agent`; `None` when it has no print mode
/// (editor-embedded, or not yet mapped).
#[must_use]
pub fn headless_cli(agent: Agent) -> Option<HeadlessCli> {
    let cli = match agent {
        Agent::ClaudeCode => HeadlessCli {
            print: &["-p"],
            autonomous: &["--dangerously-skip-permissions"],
            // Claude Code rejects `stream-json` in print mode without `--verbose`.
            structured: Some(Structured {
                args: &["--output-format", "stream-json", "--verbose"],
                format: ReplyFormat::ResultObject,
                ends_at_result: true,
            }),
            resume: Some("--resume"),
            discard: None,
        },
        // `cursor-agent -p`: "Print responses to console (for scripts or
        // non-interactive use)". Headless: `--force` auto-allows commands,
        // `--trust` skips the workspace-trust prompt, `--approve-mcps` skips
        // MCP approval. No `--sandbox`: jobs already run under agentflare's
        // bwrap, and a second sandbox would double-constrain.
        Agent::Cursor => HeadlessCli {
            print: &["-p"],
            autonomous: &["--force", "--trust", "--approve-mcps"],
            structured: Some(Structured {
                args: &["--output-format", "stream-json"],
                format: ReplyFormat::ResultObject,
                ends_at_result: false,
            }),
            resume: Some("--resume"),
            discard: None,
        },
        // `agy -p`: "Run a single prompt non-interactively and print the
        // response". Only whole-reply `json` is verified for agy.
        Agent::Antigravity => HeadlessCli {
            print: &["-p"],
            autonomous: &["--dangerously-skip-permissions"],
            structured: Some(Structured {
                args: &["--output-format", "json"],
                format: ReplyFormat::ResultObject,
                ends_at_result: false,
            }),
            resume: Some("--conversation"),
            discard: None,
        },
        Agent::Codex => HeadlessCli {
            print: &["exec"],
            // Keep Codex's own sandbox even where bwrap is unavailable
            // (Windows/macOS): explicit exec options replace `--full-auto`
            // without granting unrestricted host access or prompting.
            autonomous: &[
                "--sandbox",
                "workspace-write",
                "-c",
                "approval_policy=\"never\"",
            ],
            structured: Some(Structured {
                args: &["--json"],
                format: ReplyFormat::CodexEvents,
                ends_at_result: false,
            }),
            // `codex exec resume <id>`.
            resume: Some("resume"),
            discard: None,
        },
        Agent::GeminiCli => HeadlessCli {
            print: &["-p"],
            autonomous: &["--yolo"],
            structured: None,
            resume: None,
            discard: None,
        },
        // `opencode run --auto`: "auto-approve permissions that are not
        // explicitly denied". `--format json`: "raw JSON events";
        // `--session`: "session id to continue" (opencode 1.19).
        Agent::Opencode => HeadlessCli {
            print: &["run"],
            autonomous: &["--auto"],
            structured: Some(Structured {
                args: &["--format", "json"],
                format: ReplyFormat::OpencodeEvents,
                ends_at_result: false,
            }),
            resume: Some("--session"),
            // `opencode session delete <sessionID>` (1.19): non-interactive.
            discard: Some(&["session", "delete"]),
        },
        // No subcommand: `cline` starts in act mode and goes headless when
        // stdin is piped (docs.cline.bot/usage/cli-overview). `--auto-approve
        // true` is passed explicitly so a local config or a future default
        // can't silently turn a dispatch interactive.
        Agent::Cline => HeadlessCli {
            print: &[],
            autonomous: &["--auto-approve", "true"],
            structured: None,
            resume: None,
            discard: None,
        },
        _ => return None,
    };
    Some(cli)
}

/// Print-mode flags; `None` when the agent has no headless invocation.
#[must_use]
pub fn headless_args(agent: Agent) -> Option<&'static [&'static str]> {
    headless_cli(agent).map(|c| c.print)
}

/// Unattended-mode flags for `agentflare work`; `None` when the agent has
/// no headless row (so it can't be dispatched).
#[must_use]
pub fn autonomous_args(agent: Agent) -> Option<&'static [&'static str]> {
    headless_cli(agent).map(|c| c.autonomous)
}

/// Structured-output mode; `None` when the reply shape isn't verified.
#[must_use]
pub fn structured_output(agent: Agent) -> Option<Structured> {
    headless_cli(agent).and_then(|c| c.structured)
}

/// The long flags from `agent`'s row (autonomous + structured) that `argv`
/// actually passes: what `--help` must still list. Flags the caller supplied
/// itself are not ours to vouch for, so a user typo is never blamed on the
/// agent. Derived from the row, so there is no second list to keep in sync.
#[must_use]
pub fn required_flags(agent: Agent, argv: &[String]) -> Vec<&'static str> {
    let Some(cli) = headless_cli(agent) else {
        return Vec::new();
    };
    cli.autonomous
        .iter()
        .chain(cli.structured.iter().flat_map(|s| s.args))
        .copied()
        .filter(|f| f.starts_with("--") && argv.iter().any(|a| a == f))
        .collect()
}

/// Args that resume `session_id` (`--resume <id>`, `resume <id>` for
/// Codex); empty when the agent can't resume.
#[must_use]
pub fn resume_args(agent: Agent, session_id: &str) -> Vec<String> {
    headless_cli(agent)
        .and_then(|c| c.resume)
        .map(|flag| vec![flag.to_string(), session_id.to_string()])
        .unwrap_or_default()
}

/// Argv that deletes `session_id`; `None` when the agent has no verified
/// delete command.
#[must_use]
pub fn discard_args(agent: Agent, session_id: &str) -> Option<Vec<String>> {
    let base = headless_cli(agent)?.discard?;
    Some(
        base.iter()
            .map(|a| (*a).to_string())
            .chain([session_id.to_string()])
            .collect(),
    )
}

/// Best-effort delete of `session_id` after a turn was killed: never fails
/// the caller, bounded by `DISCARD_TIMEOUT`, logs on error.
pub fn discard_session(agent: Agent, session_id: &str) {
    let Some(cmd_argv) = discard_args(agent, session_id) else {
        return;
    };
    let spec = crate::registry::spec(agent);
    let Some(binary) = crate::detect::find_binary(spec.binary_names) else {
        return;
    };
    let arg_refs: Vec<&str> = cmd_argv.iter().map(String::as_str).collect();
    match crate::detect::run_with_timeout(&binary, &arg_refs, DISCARD_TIMEOUT) {
        Ok((true, _)) => {}
        Ok((false, out)) => eprintln!(
            "note: discarding {} session {session_id} failed: {}",
            spec.display_name,
            out.trim()
        ),
        Err(e) => eprintln!(
            "note: discarding {} session {session_id}: {e}",
            spec.display_name
        ),
    }
}

const DISCARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// True when `args` already choose an output format, so a caller's pin is
/// never doubled (a second `--output-format` once lost every session id:
/// audit 2026-09-24, finding #1).
#[must_use]
pub fn pins_output_format(args: &[String]) -> bool {
    args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--output-format" | "--format" | "--stream-json" | "--json"
        ) || a.starts_with("--output-format=")
            || a.starts_with("--format=")
    })
}

/// True when the format `args` finally select (last one wins) is JSON, so
/// stdout is a structured transcript rather than plain reply text.
#[must_use]
pub fn selects_structured_output(args: &[String]) -> bool {
    args.iter()
        .enumerate()
        .filter_map(|(i, arg)| match arg.as_str() {
            "--json" | "--stream-json" => Some("json"),
            "--output-format" | "--format" => args.get(i + 1).map(String::as_str),
            _ => arg
                .strip_prefix("--output-format=")
                .or_else(|| arg.strip_prefix("--format=")),
        })
        .next_back()
        .is_some_and(|format| matches!(format, "json" | "stream-json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_args_map_known_print_modes() {
        assert_eq!(headless_args(Agent::ClaudeCode), Some(&["-p"][..]));
        assert_eq!(headless_args(Agent::Codex), Some(&["exec"][..]));
        assert_eq!(headless_args(Agent::GeminiCli), Some(&["-p"][..]));
        assert_eq!(headless_args(Agent::Opencode), Some(&["run"][..]));
        assert_eq!(headless_args(Agent::Cursor), Some(&["-p"][..]));
        assert_eq!(headless_args(Agent::Antigravity), Some(&["-p"][..]));
        assert_eq!(headless_args(Agent::Cline), Some(&[][..]));
    }

    #[test]
    fn headless_args_none_for_agents_without_a_print_mode() {
        assert_eq!(headless_args(Agent::Windsurf), None);
        assert_eq!(headless_args(Agent::VscodeCopilot), None);
    }

    #[test]
    fn autonomous_args_maps_cursor_to_force_trust_and_approve_mcps() {
        assert_eq!(
            autonomous_args(Agent::Cursor),
            Some(&["--force", "--trust", "--approve-mcps"][..])
        );
        assert_eq!(
            autonomous_args(Agent::Antigravity),
            Some(&["--dangerously-skip-permissions"][..])
        );
    }

    #[test]
    fn autonomous_args_keeps_codex_sandbox_without_approval_prompts() {
        assert_eq!(
            autonomous_args(Agent::Codex),
            Some(
                &[
                    "--sandbox",
                    "workspace-write",
                    "-c",
                    "approval_policy=\"never\""
                ][..]
            )
        );
    }

    #[test]
    fn autonomous_args_maps_cline_to_explicit_auto_approve() {
        assert_eq!(
            autonomous_args(Agent::Cline),
            Some(&["--auto-approve", "true"][..])
        );
    }

    #[test]
    fn structured_output_streams_where_verified() {
        let args = |a| structured_output(a).map(|s| s.args);
        assert_eq!(
            args(Agent::ClaudeCode),
            Some(&["--output-format", "stream-json", "--verbose"][..])
        );
        assert_eq!(
            args(Agent::Cursor),
            Some(&["--output-format", "stream-json"][..])
        );
        assert_eq!(
            args(Agent::Antigravity),
            Some(&["--output-format", "json"][..])
        );
        assert_eq!(args(Agent::Codex), Some(&["--json"][..]));
        assert_eq!(args(Agent::Opencode), Some(&["--format", "json"][..]));
        assert_eq!(args(Agent::GeminiCli), None, "flags unverified");
        assert_eq!(
            structured_output(Agent::Codex).map(|s| s.format),
            Some(ReplyFormat::CodexEvents)
        );
        assert_eq!(
            structured_output(Agent::Opencode).map(|s| s.format),
            Some(ReplyFormat::OpencodeEvents)
        );
    }

    #[test]
    fn every_structured_mode_is_recognised_as_selected() {
        for agent in [
            Agent::ClaudeCode,
            Agent::Cursor,
            Agent::Antigravity,
            Agent::Codex,
            Agent::Opencode,
        ] {
            let args: Vec<String> = structured_output(agent)
                .unwrap()
                .args
                .iter()
                .map(|s| s.to_string())
                .collect();
            assert!(pins_output_format(&args), "{agent:?}");
            assert!(selects_structured_output(&args), "{agent:?}");
        }
    }

    #[test]
    fn selects_structured_output_honours_the_last_format() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(selects_structured_output(&v(&["--output-format", "json"])));
        assert!(selects_structured_output(&v(&[
            "--output-format=stream-json"
        ])));
        assert!(selects_structured_output(&v(&["--format=json"])));
        assert!(!selects_structured_output(&v(&[
            "--output-format=json",
            "--output-format",
            "text"
        ])));
        assert!(!selects_structured_output(&v(&["--format", "default"])));
        assert!(!selects_structured_output(&[]));
    }

    #[test]
    fn required_flags_cover_only_row_flags_actually_passed() {
        let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let argv = v(&["--sandbox", "workspace-write", "--json", "--typo"]);
        assert_eq!(required_flags(Agent::Codex, &argv), ["--sandbox", "--json"]);
        assert_eq!(
            required_flags(Agent::Codex, &v(&["--typo"])),
            [] as [&str; 0]
        );
        assert!(required_flags(Agent::Windsurf, &argv).is_empty());
    }

    #[test]
    fn resume_args_maps_supported_cli_modes() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(resume_args(Agent::ClaudeCode, "s1"), v(&["--resume", "s1"]));
        assert_eq!(resume_args(Agent::Cursor, "s1"), v(&["--resume", "s1"]));
        assert_eq!(
            resume_args(Agent::Antigravity, "s1"),
            v(&["--conversation", "s1"])
        );
        assert_eq!(resume_args(Agent::Codex, "s1"), v(&["resume", "s1"]));
        assert_eq!(resume_args(Agent::Opencode, "s1"), v(&["--session", "s1"]));
        assert!(resume_args(Agent::GeminiCli, "s1").is_empty());
    }

    #[test]
    fn discard_args_only_for_agents_with_a_verified_delete() {
        assert_eq!(
            discard_args(Agent::Opencode, "ses_1"),
            Some(vec!["session".into(), "delete".into(), "ses_1".into()])
        );
        assert_eq!(discard_args(Agent::ClaudeCode, "s1"), None);
    }

    #[test]
    fn pins_output_format_recognises_every_spelling() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(pins_output_format(&v(&["--output-format", "json"])));
        assert!(pins_output_format(&v(&["--output-format=stream-json"])));
        assert!(pins_output_format(&v(&["--stream-json"])));
        assert!(pins_output_format(&v(&["--json"])));
        assert!(pins_output_format(&v(&["--format", "default"])));
        assert!(pins_output_format(&v(&["--format=json"])));
        assert!(!pins_output_format(&v(&["--model", "x"])));
    }
}
