use clap::{Args, Subcommand};
use std::io::Read;

fn subagent_should_inject() -> bool {
    let matcher = match std::env::var("PONYTAIL_SUBAGENT_MATCHER") {
        Ok(m) => m,
        Err(_) => return true,
    };

    let re = match regex::Regex::new(&format!("(?i){matcher}")) {
        Ok(r) => r,
        Err(_) => {
            eprintln!("[ponytail] invalid PONYTAIL_SUBAGENT_MATCHER regex — injecting everywhere");
            return true;
        }
    };

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
    if agent_type.is_empty() {
        return true;
    }

    re.is_match(agent_type)
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
                            ponytail::SwitchAction::Report => {}
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
