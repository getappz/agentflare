use clap::{Args, Subcommand};

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

impl PonytailArgs {
    pub fn run(self) {
        match self.action {
            PonytailAction::Status => {
                let mode = ponytail::active_mode().unwrap_or_else(ponytail::default_mode);
                println!("{mode}");
            }
            PonytailAction::Set { mode } => {
                let normalized = ponytail::normalize_config_mode(&mode)
                    .unwrap_or("full");
                ponytail::set_active(normalized).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                });
                println!("{normalized}");
            }
            PonytailAction::Default { mode } => {
                ponytail::set_default_mode(&mode).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                });
                ponytail::set_active(&mode).ok();
                println!("default: {mode}");
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
                    let mode = ponytail::active_mode()
                        .unwrap_or_else(ponytail::default_mode);
                    if mode == "off" {
                        ponytail::state::clear_active();
                        println!("OK");
                        return;
                    }
                    ponytail::set_active(&mode).ok();
                    let instructions = ponytail::build_instructions(&mode, None);
                    let platform = ponytail::detect_platform();
                    let output = ponytail::format_hook_output(
                        "SessionStart",
                        &instructions.body,
                        &platform,
                    );
                    println!("{output}");
                }
                PonytailHookEvent::SubagentStart => {
                    let mode = ponytail::active_mode()
                        .unwrap_or_else(ponytail::default_mode);
                    if mode == "off" {
                        println!("OK");
                        return;
                    }
                    let instructions = ponytail::build_instructions(&mode, None);
                    let platform = ponytail::detect_platform();
                    let output = ponytail::format_hook_output(
                        "SubagentStart",
                        &instructions.body,
                        &platform,
                    );
                    println!("{output}");
                }
                PonytailHookEvent::PromptSubmit => {
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).ok();
                    if let Some(action) = ponytail::detect_switch(&input) {
                        match action {
                            ponytail::SwitchAction::SetMode(m) => {
                                ponytail::set_active(&m).ok();
                            }
                            ponytail::SwitchAction::SetDefault(m) => {
                                ponytail::set_default_mode(&m).ok();
                                ponytail::set_active(&m).ok();
                            }
                            ponytail::SwitchAction::Off => {
                                ponytail::clear_active();
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
