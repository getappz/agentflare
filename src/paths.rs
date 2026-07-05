// Home-directory resolution with an explicit test override. `dirs::home_dir()`
// resolves via the OS directly on Windows (SHGetKnownFolderPath) and ignores
// HOME/USERPROFILE env var overrides — learned the hard way when a
// "sandboxed" test run wrote real changes to a live ~/.claude/settings.json.
// LEANSTACK_HOME_OVERRIDE is leanstack's own escape hatch for tests/CI.
use std::path::PathBuf;

pub fn home() -> PathBuf {
    if let Ok(p) = std::env::var("LEANSTACK_HOME_OVERRIDE") {
        return PathBuf::from(p);
    }
    dirs::home_dir().expect("home directory not found")
}
