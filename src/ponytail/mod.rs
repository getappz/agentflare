pub mod config;
pub mod detect;
pub mod instructions;
pub mod platform;
pub mod state;
pub mod sub_skills;
pub mod switcher;

pub use config::{default_mode, normalize_config_mode, set_default_mode};
pub use instructions::{build as build_instructions, download_skill};
pub use platform::{detect_platform, format_hook_output};
pub use state::{active_mode, clear_active, set_active};
pub use switcher::{detect as detect_switch, SwitchAction};
