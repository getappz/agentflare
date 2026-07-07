pub mod config;
pub mod instructions;
pub mod platform;
pub mod state;
pub mod switcher;

pub use config::{
    default_mode, is_deactivation, normalize_config_mode, normalize_mode,
    normalize_persisted_mode, set_default_mode, DEFAULT_MODE, RUNTIME_MODES, VALID_MODES,
};
pub use instructions::{build as build_instructions, fallback_instructions, Instructions};
pub use platform::{detect as detect_platform, format_hook_output, AgentPlatform};
pub use state::{active_mode, clear_active, set_active};
pub use switcher::{detect as detect_switch, SwitchAction};
