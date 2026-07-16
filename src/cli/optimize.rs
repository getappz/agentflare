use clap::{Args, Subcommand};
use std::io::Read;

// ---------------------------------------------------------------------------
// Output compression subcommand (was caveman)
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum OutputAction {
    /// Compress a markdown file using LLM-based prose compression.
    Compress {
        source: std::path::PathBuf,
        /// Defaults to `source` (in-place) when omitted.
        target: Option<std::path::PathBuf>,
        #[arg(long)]
        spec_file: Option<std::path::PathBuf>,
        #[arg(long)]
        backup: Option<String>,
    },
}

/// Register an output-layer compression's original and return its expand-marker.
fn record_and_marker(
    original_path: std::path::PathBuf,
    before: u64,
    after: u64,
    now: u64,
) -> String {
    let entry = crate::optimize::retrieve::register(
        crate::optimize::retrieve::EntryKind::FileBackup {
            backup_path: original_path,
        },
        before,
        after,
        now,
    );
    crate::optimize::retrieve::marker(&entry)
}

impl OutputAction {
    fn run(self) {
        match self {
            OutputAction::Compress {
                source,
                target,
                spec_file,
                backup,
            } => {
                let target = target.unwrap_or_else(|| source.clone());
                let prompt = match &spec_file {
                    Some(path) => match std::fs::read_to_string(path) {
                        Ok(spec) => crate::optimize::output::Prompt::Custom(spec),
                        Err(e) => {
                            eprintln!("failed to read spec file {}: {e}", path.display());
                            std::process::exit(1);
                        }
                    },
                    None => crate::optimize::output::Prompt::Generic,
                };
                let backup_mode = match backup.as_deref() {
                    Some("sibling") => crate::optimize::output::BackupMode::Sibling,
                    Some("out-of-tree") | None => crate::optimize::output::BackupMode::OutOfTree,
                    Some(other) => {
                        eprintln!("--backup must be 'sibling' or 'out-of-tree', got '{other}'");
                        std::process::exit(1);
                    }
                };
                let result = crate::optimize::output::compress(
                    &crate::optimize::output::RealLlm,
                    &source,
                    &target,
                    prompt,
                    backup_mode,
                );
                match result {
                    Ok(report) => {
                        let pct =
                            100 - (100 * report.compressed_bytes / report.original_bytes.max(1));
                        println!(
                            "{}→{}B ▼{pct}%",
                            report.original_bytes, report.compressed_bytes
                        );
                        println!(
                            "{}",
                            record_and_marker(
                                report.original_path.clone(),
                                report.original_bytes as u64,
                                report.compressed_bytes as u64,
                                crate::optimize::retrieve::now_unix(),
                            )
                        );
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Code minimalism subcommand (was ponytail)
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum CodeAction {
    Status,
    Set {
        mode: String,
    },
    Default {
        mode: String,
    },
    Off,
    Review,
    Audit,
    Debt,
    Gain,
    Info,
    Playbook,
    NoHallucination,
    Hook {
        #[command(subcommand)]
        event: CodeHookEvent,
    },
}

#[derive(Subcommand)]
pub enum CodeHookEvent {
    SessionStart,
    SubagentStart,
    PromptSubmit,
    Statusline,
}

fn code_emit_hook(event: &str, off_guard: bool) {
    let mode =
        crate::optimize::code::active_mode().unwrap_or_else(crate::optimize::code::default_mode);
    if off_guard && mode == "off" {
        crate::optimize::code::clear_active();
        println!("OK");
        return;
    }
    let instructions = crate::optimize::code::build_instructions(&mode, None);
    let platform = crate::optimize::code::detect_platform();
    let output = crate::optimize::code::format_hook_output(event, &instructions.body, &platform);
    println!("{output}");
}

const DEFAULT_EXCLUDE_AGENT_TYPES: &str =
    "explore|investigat|search|review|readonly|read-only|verify";

fn code_should_inject_for(agent_type: &str, override_matcher: Option<&str>) -> bool {
    if agent_type.is_empty() {
        return true;
    }
    let (pattern, is_allowlist) = match override_matcher {
        Some(m) => (m, true),
        None => (DEFAULT_EXCLUDE_AGENT_TYPES, false),
    };
    let re = match regex::Regex::new(&format!("(?i){pattern}")) {
        Ok(r) => r,
        Err(_) => {
            eprintln!(
                "[flare code] invalid FLARE_CODE_SUBAGENT_MATCHER regex — injecting everywhere"
            );
            return true;
        }
    };
    let matched = re.is_match(agent_type);
    if is_allowlist { matched } else { !matched }
}

fn code_subagent_should_inject() -> bool {
    let override_matcher = std::env::var("FLARE_CODE_SUBAGENT_MATCHER")
        .or_else(|_| std::env::var("PONYTAIL_SUBAGENT_MATCHER"))
        .ok();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut input = String::new();
        let _ = std::io::stdin().read_to_string(&mut input);
        let _ = tx.send(input);
    });
    let input = match rx.recv_timeout(std::time::Duration::from_millis(1000)) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[flare code] SubagentStart stdin timeout — injecting");
            return true;
        }
    };
    let data: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(_) => return true,
    };
    let agent_type = data
        .get("agent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    code_should_inject_for(agent_type, override_matcher.as_deref())
}

fn code_report_message(mode: &str) -> String {
    if mode == "off" {
        "flare code is off. Use /flare code lite|full|ultra to activate.".to_string()
    } else {
        format!("FLARE CODE MODE ACTIVE — level: {mode}")
    }
}

impl CodeAction {
    fn run(self) {
        match self {
            CodeAction::Status => {
                let mode = crate::optimize::code::active_mode()
                    .unwrap_or_else(crate::optimize::code::default_mode);
                println!("{mode}");
            }
            CodeAction::Set { mode } => {
                let normalized = crate::optimize::code::normalize_config_mode(&mode)
                    .unwrap_or_else(|| {
                        eprintln!("error: invalid mode: {mode}");
                        std::process::exit(1);
                    });
                crate::optimize::code::set_active(normalized).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                });
                println!("{normalized}");
            }
            CodeAction::Default { mode } => {
                let normalized = crate::optimize::code::normalize_config_mode(&mode)
                    .unwrap_or_else(|| {
                        eprintln!("error: invalid mode: {mode}");
                        std::process::exit(1);
                    });
                crate::optimize::code::set_default_mode(normalized).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                });
                crate::optimize::code::set_active(normalized).ok();
                println!("default: {normalized}");
            }
            CodeAction::Off => {
                crate::optimize::code::clear_active();
                println!("off");
            }
            CodeAction::Review => {
                println!("{}", crate::optimize::code::SKILL_REVIEW);
            }
            CodeAction::Audit => {
                println!("{}", crate::optimize::code::SKILL_AUDIT);
            }
            CodeAction::Debt => {
                println!("{}", crate::optimize::code::SKILL_DEBT);
            }
            CodeAction::Gain => {
                println!("{}", crate::optimize::code::SKILL_GAIN);
            }
            CodeAction::Info => {
                println!("{}", crate::optimize::code::SKILL_HELP);
            }
            CodeAction::Playbook => {
                println!("{}", crate::optimize::code::SKILL_PLAYBOOK);
            }
            CodeAction::NoHallucination => {
                println!("{}", crate::optimize::code::SKILL_NO_HALLUCINATION);
            }
            CodeAction::Hook { event } => match event {
                CodeHookEvent::SessionStart => {
                    crate::optimize::code::clear_session();
                    let mode = crate::optimize::code::active_mode()
                        .unwrap_or_else(crate::optimize::code::default_mode);
                    if mode != "off" {
                        crate::optimize::code::set_active(&mode).ok();
                    }
                    code_emit_hook("SessionStart", true);
                }
                CodeHookEvent::SubagentStart => {
                    if code_subagent_should_inject() {
                        code_emit_hook("SubagentStart", true);
                    }
                }
                CodeHookEvent::PromptSubmit => {
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).ok();
                    if let Some(action) = crate::optimize::code::detect_switch_action(&input) {
                        match action {
                            crate::optimize::code::SwitchAction::SetMode(m) => {
                                crate::optimize::code::set_active(&m).ok();
                            }
                            crate::optimize::code::SwitchAction::SetSession(m) => {
                                crate::optimize::code::set_session(&m).ok();
                            }
                            crate::optimize::code::SwitchAction::SetDefault(m) => {
                                crate::optimize::code::set_default_mode(&m).ok();
                                crate::optimize::code::set_active(&m).ok();
                            }
                            crate::optimize::code::SwitchAction::Off => {
                                crate::optimize::code::clear_active();
                            }
                            crate::optimize::code::SwitchAction::Report => {
                                let mode = crate::optimize::code::active_mode()
                                    .unwrap_or_else(crate::optimize::code::default_mode);
                                let platform = crate::optimize::code::detect_platform();
                                let ctx = code_report_message(&mode);
                                let output = crate::optimize::code::format_hook_output(
                                    "UserPromptSubmit",
                                    &ctx,
                                    &platform,
                                );
                                println!("{output}");
                                return;
                            }
                        }
                    }
                    println!("OK");
                }
                CodeHookEvent::Statusline => {
                    let mode = crate::optimize::code::active_mode()
                        .unwrap_or_else(crate::optimize::code::default_mode);
                    if mode == "off" || mode.is_empty() {
                        return;
                    }
                    if mode == "full" {
                        print!("\x1b[38;5;108m[FLARE-CODE]\x1b[0m");
                    } else {
                        let upper = mode.to_uppercase();
                        print!("\x1b[38;5;108m[FLARE-CODE:{upper}]\x1b[0m");
                    }
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level OptimizeArgs
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum FlareAction {
    /// Output compression (was caveman) — LLM-based prose compression
    Output {
        #[command(subcommand)]
        action: OutputAction,
    },
    /// Code minimalism (was ponytail) — lazy senior dev mode
    Code {
        #[command(subcommand)]
        action: CodeAction,
    },
    /// Context compaction — session transcript relevance scoring
    Context {
        #[command(subcommand)]
        action: ContextAction,
    },
    /// Show flare system status
    Status,
    /// Retrieve an original that the output layer compressed away (CCR)
    Retrieve {
        /// Registered id (e.g. r-a1b2c3); omit and pass --list to enumerate
        id: Option<String>,
        #[arg(long)]
        list: bool,
    },
}

#[derive(Subcommand)]
pub enum ContextAction {
    /// Score transcript lines by BM25 relevance to a query
    Score {
        /// Path to transcript file
        transcript: std::path::PathBuf,
        /// Relevance query (defaults to last line of transcript)
        query: Option<String>,
    },
}

#[derive(Args)]
pub struct OptimizeArgs {
    #[command(subcommand)]
    pub action: FlareAction,
}

fn retrieve_list_line(e: &crate::optimize::retrieve::CompressionEntry) -> String {
    format!(
        "{}\t{}\u{2192}{}\t{}",
        e.id,
        e.size_before,
        e.size_after,
        crate::optimize::retrieve::describe_origin(&e.kind)
    )
}

impl OptimizeArgs {
    pub fn run(self) {
        match self.action {
            FlareAction::Output { action } => action.run(),
            FlareAction::Code { action } => action.run(),
            FlareAction::Context { ref action } => self.run_context(action),
            FlareAction::Status => self.run_status(),
            FlareAction::Retrieve { ref id, list } => self.run_retrieve(id.as_deref(), list),
        }
    }

    fn run_context(&self, action: &ContextAction) {
        match action {
            ContextAction::Score { transcript, query } => {
                let content = match std::fs::read_to_string(&transcript) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("error reading transcript: {e}");
                        std::process::exit(1);
                    }
                };
                let entries: Vec<crate::optimize::context::LineEntry> = content
                    .lines()
                    .enumerate()
                    .map(|(i, text)| crate::optimize::context::LineEntry {
                        index: i,
                        text: text.to_string(),
                    })
                    .collect();
                let q = query.clone().unwrap_or_else(|| {
                    content
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .to_string()
                });
                match crate::optimize::context::score_lines(&entries, &q) {
                    Ok(scored) => {
                        if let Ok(json) = serde_json::to_string(&scored) {
                            println!("{json}");
                        }
                    }
                    Err(e) => {
                        eprintln!("scoring error: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    fn run_status(&self) {
        let output_mode = "available"; // always available when compiled
        let code_mode = crate::optimize::code::active_mode()
            .unwrap_or_else(crate::optimize::code::default_mode);
        let runtime_turns = crate::optimize::load_runtime()
            .sessions
            .iter()
            .map(|(_, r)| r.turn_count)
            .sum::<u32>();
        println!(
            "FLARE OPTIMIZE ACTIVE\n\
             output:  {output_mode}\n\
             code:    {code_mode}\n\
             context: available (FTS5/BM25)\n\
             runtime: {runtime_turns} total turns tracked"
        );
    }

    fn run_retrieve(&self, id: Option<&str>, list: bool) {
        use crate::optimize::retrieve;
        if list {
            let state = retrieve::active_state(retrieve::now_unix());
            let mut entries: Vec<_> = state.entries.values().collect();
            entries.sort_by_key(|e| std::cmp::Reverse(e.created_ts));
            for e in entries {
                println!("{}", retrieve_list_line(e));
            }
            return;
        }
        match id {
            Some(id) => match retrieve::retrieve(id) {
                Ok(content) => print!("{content}"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("provide an id, or pass --list to enumerate");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_report_message_says_active_for_runtime_mode() {
        assert_eq!(
            code_report_message("full"),
            "FLARE CODE MODE ACTIVE — level: full"
        );
    }

    #[test]
    fn code_should_inject_for_excludes_read_only_agent_types_by_default() {
        assert!(!code_should_inject_for("cavecrew-investigator", None));
        assert!(!code_should_inject_for("Explore", None));
        assert!(!code_should_inject_for("cavecrew-reviewer", None));
        assert!(!code_should_inject_for("some-search-agent", None));
    }

    #[test]
    fn code_should_inject_for_includes_code_writing_agent_types_by_default() {
        assert!(code_should_inject_for("general-purpose", None));
        assert!(code_should_inject_for("cavecrew-builder", None));
    }

    #[test]
    fn code_should_inject_for_treats_empty_agent_type_as_inject() {
        assert!(code_should_inject_for("", None));
        assert!(code_should_inject_for("", Some("builder")));
    }

    #[test]
    fn code_should_inject_for_override_matcher_is_an_allowlist_not_a_denylist() {
        assert!(code_should_inject_for("explore", Some("explore|builder")));
        assert!(!code_should_inject_for("other", Some("explore|builder")));
    }

    #[test]
    fn code_should_inject_for_falls_back_to_inject_on_invalid_override_regex() {
        assert!(code_should_inject_for("anything", Some("[invalid(")));
    }

    #[test]
    fn code_report_message_says_off_for_off_mode() {
        assert_eq!(
            code_report_message("off"),
            "flare code is off. Use /flare code lite|full|ultra to activate."
        );
    }

    #[test]
    fn record_and_marker_registers_a_retrievable_original() {
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            let backup = crate::state::state_dir().join("orig.md");
            std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
            std::fs::write(&backup, "ORIGINAL").unwrap();

            let m = super::record_and_marker(backup.clone(), 100, 10, 1_000);
            assert!(m.contains("optimize retrieve"), "marker text: {m}");

            // the first r-xxxxxx token in the marker is the id; it must be retrievable
            let id = m
                .split_whitespace()
                .find(|w| w.starts_with("r-"))
                .expect("marker should contain an r- id");
            assert_eq!(crate::optimize::retrieve::retrieve(id).unwrap(), "ORIGINAL");
        });
    }

    #[test]
    fn retrieve_list_line_shows_id_sizes_and_origin() {
        use crate::optimize::retrieve::{CompressionEntry, EntryKind};
        let e = CompressionEntry {
            id: "r-abc123".into(),
            kind: EntryKind::FileBackup {
                backup_path: "/tmp/x.md".into(),
            },
            size_before: 100,
            size_after: 10,
            created_ts: 0,
        };
        let line = super::retrieve_list_line(&e);
        assert!(line.contains("r-abc123"), "line: {line}");
        assert!(line.contains("100") && line.contains("10"), "line: {line}");
        assert!(line.contains("file:/tmp/x.md"), "line: {line}");
    }
}
