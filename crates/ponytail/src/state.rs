use std::io;
use std::path::PathBuf;

pub fn flag_path() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("agentflare")
        .join("ponytail")
        .join("active")
}

pub fn session_path() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("agentflare")
        .join("ponytail")
        .join("session-mode")
}

fn read_session() -> Option<String> {
    std::fs::read_to_string(session_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn active_mode() -> Option<String> {
    read_session().or_else(|| {
        std::fs::read_to_string(flag_path())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

pub fn set_active(mode: &str) -> io::Result<()> {
    let path = flag_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, mode)
}

pub fn set_session(mode: &str) -> io::Result<()> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, mode)
}

pub fn clear_session() {
    let _ = std::fs::remove_file(session_path());
}

pub fn clear_active() {
    let _ = std::fs::remove_file(flag_path());
    clear_session();
}

pub fn active_scope() -> &'static str {
    if read_session().is_some() {
        "session"
    } else {
        "global"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_active_mode() {
        clear_active();
        assert_eq!(active_mode(), None);

        set_active("full").unwrap();
        assert_eq!(active_mode(), Some("full".to_string()));
        assert_eq!(active_scope(), "global");

        set_session("ultra").unwrap();
        assert_eq!(active_mode(), Some("ultra".to_string()));
        assert_eq!(active_scope(), "session");

        clear_session();
        assert_eq!(active_mode(), Some("full".to_string()));
        assert_eq!(active_scope(), "global");

        clear_active();
        assert_eq!(active_mode(), None);
    }

    #[test]
    fn clear_nonexistent_is_noop() {
        clear_active();
        clear_active();
        clear_session();
    }
}
