//! Flare code minimalism layer — lazy senior dev rules for AI agents.
pub use flare_code::config::config_path;
pub use flare_code::sub_skills::{
    SKILL_AUDIT, SKILL_DEBT, SKILL_GAIN, SKILL_HELP, SKILL_NO_HALLUCINATION, SKILL_PLAYBOOK,
    SKILL_REVIEW,
};
pub use flare_code::switcher::SwitchAction;
pub use flare_code::switcher::detect as detect_switch_action;
pub use flare_code::{
    active_mode, build_instructions, clear_active, clear_session, default_mode, detect_platform,
    format_hook_output, hide_status, normalize_config_mode, set_active, set_default_mode,
    set_session, sub_skills,
};
