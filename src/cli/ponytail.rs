//! DEPRECATED — use `agentflare flare code` instead.
use clap::{Args, Subcommand};
use std::io::Read;

const DEFAULT_EXCLUDE_AGENT_TYPES: &str =
    "explore|investigat|search|review|readonly|read-only|verify";

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

impl PonytailArgs {
    pub fn run(self) {
        match self.action {
            PonytailAction::Status => {
                let mode = crate::optimize::code::active_mode()
                    .unwrap_or_else(crate::optimize::code::default_mode);
                println!("{mode}");
            }
            PonytailAction::Set { mode } => {
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
            PonytailAction::Default { mode } => {
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
            PonytailAction::Off => {
                crate::optimize::code::clear_active();
                println!("off");
            }
            PonytailAction::Review => println!("{}", crate::optimize::code::SKILL_REVIEW),
            PonytailAction::Audit => println!("{}", crate::optimize::code::SKILL_AUDIT),
            PonytailAction::Debt => println!("{}", crate::optimize::code::SKILL_DEBT),
            PonytailAction::Gain => println!("{}", crate::optimize::code::SKILL_GAIN),
            PonytailAction::Info => println!("{}", crate::optimize::code::SKILL_HELP),
            PonytailAction::Playbook => println!("{}", crate::optimize::code::SKILL_PLAYBOOK),
            PonytailAction::NoHallucination => {
                println!("{}", crate::optimize::code::SKILL_NO_HALLUCINATION)
            }
            PonytailAction::Hook { event } => match event {
                PonytailHookEvent::SessionStart => {
                    crate::optimize::code::clear_session();
                    let mode = crate::optimize::code::active_mode()
                        .unwrap_or_else(crate::optimize::code::default_mode);
                    if mode != "off" {
                        crate::optimize::code::set_active(&mode).ok();
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
                                let ctx = report_message(&mode);
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
                PonytailHookEvent::Statusline => {
                    let mode = crate::optimize::code::active_mode()
                        .unwrap_or_else(crate::optimize::code::default_mode);
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
    fn should_inject_for_excludes_read_only_agent_types_by_default() {
        assert!(!should_inject_for("cavecrew-investigator", None));
        assert!(!should_inject_for("Explore", None));
        assert!(!should_inject_for("cavecrew-reviewer", None));
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
    fn should_inject_for_falls_back_to_inject_on_invalid_override_regex() {
        assert!(should_inject_for("anything", Some("[invalid(")));
    }
}
