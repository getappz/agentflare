use clap::{Args, Subcommand};
use std::io::Read;

/// Read-only/investigative subagent types get no ponytail persona by
/// default — the persona's lazy-code/commit-style/etc. guidance is dead
/// weight (~1300 tokens) for an agent that never writes code.
const DEFAULT_EXCLUDE_AGENT_TYPES: &str = "explore|investigat|search|review|readonly|read-only|verify";

/// Decides whether to inject for a given `agent_type`. With no override,
/// `DEFAULT_EXCLUDE_AGENT_TYPES` is a DENY-list (matches → skip injection).
/// `PONYTAIL_SUBAGENT_MATCHER`, when set, fully replaces that default with a
/// caller-supplied ALLOW-list regex instead (matches → inject) — same
/// semantics as before this default existed.
fn should_inject_for(agent_type: &str, override_matcher: Option<&str>) -> bool {
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
            eprintln!("[ponytail] invalid PONYTAIL_SUBAGENT_MATCHER regex — injecting everywhere");
            return true;
        }
    };
    let matched = re.is_match(agent_type);
    if is_allowlist { matched } else { !matched }
}

fn subagent_should_inject() -> bool {
    let override_matcher = std::env::var("PONYTAIL_SUBAGENT_MATCHER").ok();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut input = String::new();
        let _ = std::io::stdin().read_to_string(&mut input);
        let _ = tx.send(input);
    });
    let input = match rx.recv_timeout(std::time::Duration::from_millis(1000)) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[ponytail] SubagentStart stdin timeout — injecting");
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

    should_inject_for(agent_type, override_matcher.as_deref())
}

#[derive(Subcommand)]
pub enum PonytailAction {
    Status,
    Set {
        mode: String,
    },
    Default {
        mode: String,
    },
    Off,
    Update,
    Review,
    Audit,
    Debt,
    Gain,
    Info,
    Playbook,
    NoHallucination,
    Hook {
        #[command(subcommand)]
        event: PonytailHookEvent,
    },
}

#[derive(Subcommand)]
pub enum PonytailHookEvent {
    SessionStart,
    SubagentStart,
    PromptSubmit,
    Statusline,
}

#[derive(Args)]
pub struct PonytailArgs {
    #[command(subcommand)]
    pub action: PonytailAction,
}

fn report_message(mode: &str) -> String {
    if mode == "off" {
        "ponytail is off. Use /ponytail lite|full|ultra to activate.".to_string()
    } else {
        format!("PONYTAIL MODE ACTIVE — level: {mode}")
    }
}

fn emit_hook(event: &str, off_guard: bool) {
    let mode = ponytail::active_mode().unwrap_or_else(ponytail::default_mode);
    if off_guard && mode == "off" {
        ponytail::clear_active();
        println!("OK");
        return;
    }
    let instructions = ponytail::build_instructions(&mode, None);
    let platform = ponytail::detect_platform();
    let output = ponytail::format_hook_output(event, &instructions.body, &platform);
    println!("{output}");
}

impl PonytailArgs {
    pub fn run(self) {
        match self.action {
            PonytailAction::Status => {
                let mode = ponytail::active_mode().unwrap_or_else(ponytail::default_mode);
                println!("{mode}");
            }
            PonytailAction::Set { mode } => {
                let normalized = ponytail::normalize_config_mode(&mode)
                    .unwrap_or_else(|| {
                        eprintln!("error: invalid mode: {mode}");
                        std::process::exit(1);
                    });
                ponytail::set_active(normalized).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                });
                println!("{normalized}");
            }
            PonytailAction::Default { mode } => {
                let normalized = ponytail::normalize_config_mode(&mode)
                    .unwrap_or_else(|| {
                        eprintln!("error: invalid mode: {mode}");
                        std::process::exit(1);
                    });
                ponytail::set_default_mode(normalized).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                });
                ponytail::set_active(normalized).ok();
                println!("default: {normalized}");
            }
            PonytailAction::Off => {
                ponytail::clear_active();
                println!("off");
            }
            PonytailAction::Update => {
                match ponytail::download_skill() {
                    Ok(path) => println!("SKILL.md updated at {path}"),
                    Err(e) => {
                        eprintln!("update failed: {e}");
                        std::process::exit(1);
                    }
                }
            }
            PonytailAction::Review => {
                println!("{}", ponytail::sub_skills::SKILL_REVIEW);
            }
            PonytailAction::Audit => {
                println!("{}", ponytail::sub_skills::SKILL_AUDIT);
            }
            PonytailAction::Debt => {
                println!("{}", ponytail::sub_skills::SKILL_DEBT);
            }
            PonytailAction::Gain => {
                println!("{}", ponytail::sub_skills::SKILL_GAIN);
            }
            PonytailAction::Info => {
                println!("{}", ponytail::sub_skills::SKILL_HELP);
            }
            PonytailAction::Playbook => {
                println!("{}", ponytail::sub_skills::SKILL_PLAYBOOK);
            }
            PonytailAction::NoHallucination => {
                println!("{}", ponytail::sub_skills::SKILL_NO_HALLUCINATION);
            }
            PonytailAction::Hook { event } => match event {
                PonytailHookEvent::SessionStart => {
                    // A session-scoped override must not outlive its session:
                    // there is no SessionEnd hook, so clear it when the next
                    // session starts — otherwise active_mode() reads the stale
                    // override and set_active() below promotes it to global.
                    ponytail::clear_session();
                    let mode = ponytail::active_mode()
                        .unwrap_or_else(ponytail::default_mode);
                    if mode != "off" {
                        ponytail::set_active(&mode).ok();
                    }
                    emit_hook("SessionStart", true);
                }
                PonytailHookEvent::SubagentStart => {
                    if subagent_should_inject() {
                        emit_hook("SubagentStart", true);
                    }
                }
                PonytailHookEvent::PromptSubmit => {
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).ok();
                    if let Some(action) = ponytail::detect_switch(&input) {
                        match action {
                            ponytail::SwitchAction::SetMode(m) => {
                                ponytail::set_active(&m).ok();
                            }
                            ponytail::SwitchAction::SetSession(m) => {
                                ponytail::set_session(&m).ok();
                            }
                            ponytail::SwitchAction::SetDefault(m) => {
                                ponytail::set_default_mode(&m).ok();
                                ponytail::set_active(&m).ok();
                            }
                            ponytail::SwitchAction::Off => {
                                ponytail::clear_active();
                            }
                            ponytail::SwitchAction::Report => {
                                let mode = ponytail::active_mode()
                                    .unwrap_or_else(ponytail::default_mode);
                                let platform = ponytail::detect_platform();
                                let ctx = report_message(&mode);
                                let output = ponytail::format_hook_output(
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
                PonytailHookEvent::Statusline => {
                    let mode = ponytail::active_mode()
                        .unwrap_or_else(ponytail::default_mode);
                    if mode == "off" || mode.is_empty() {
                        return;
                    }
                    if mode == "full" {
                        print!("\x1b[38;5;108m[PONYTAIL]\x1b[0m");
                    } else {
                        let upper = mode.to_uppercase();
                        print!("\x1b[38;5;108m[PONYTAIL:{upper}]\x1b[0m");
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_message_says_active_for_runtime_mode() {
        assert_eq!(report_message("full"), "PONYTAIL MODE ACTIVE — level: full");
    }

    #[test]
    fn should_inject_for_excludes_read_only_agent_types_by_default() {
        assert!(!should_inject_for("cavecrew-investigator", None));
        assert!(!should_inject_for("Explore", None), "case-insensitive");
        assert!(!should_inject_for("cavecrew-reviewer", None));
        assert!(!should_inject_for("some-search-agent", None));
    }

    #[test]
    fn should_inject_for_includes_code_writing_agent_types_by_default() {
        assert!(should_inject_for("general-purpose", None));
        assert!(should_inject_for("cavecrew-builder", None));
    }

    #[test]
    fn should_inject_for_treats_empty_agent_type_as_inject() {
        assert!(should_inject_for("", None));
        assert!(should_inject_for("", Some("builder")));
    }

    #[test]
    fn should_inject_for_override_matcher_is_an_allowlist_not_a_denylist() {
        // PONYTAIL_SUBAGENT_MATCHER fully replaces the default deny-list —
        // an explore-type agent normally excluded by default is injected
        // when it matches the caller's allow-list.
        assert!(should_inject_for("explore", Some("explore|builder")));
        assert!(!should_inject_for("other", Some("explore|builder")));
    }

    #[test]
    fn should_inject_for_falls_back_to_inject_on_invalid_override_regex() {
        assert!(should_inject_for("anything", Some("[invalid(")));
    }

    #[test]
    fn report_message_says_off_for_off_mode() {
        assert_eq!(
            report_message("off"),
            "ponytail is off. Use /ponytail lite|full|ultra to activate."
        );
    }
}
