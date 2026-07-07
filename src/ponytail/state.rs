use std::io;
use std::path::PathBuf;

pub fn flag_path() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("agentflare")
        .join("ponytail")
        .join("active")
}

pub fn active_mode() -> Option<String> {
    std::fs::read_to_string(flag_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn set_active(mode: &str) -> io::Result<()> {
    let path = flag_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, mode)
}

pub fn clear_active() {
    let _ = std::fs::remove_file(flag_path());
}
