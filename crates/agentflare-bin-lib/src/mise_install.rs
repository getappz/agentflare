// Cross-platform detect-or-install for mise (github.com/jdx/mise), a dev-tool
// version manager. `agentflare run` uses it to launch agents with mise-managed
// tools on PATH for the session, on machines that don't already have mise.
use crate::paths::home;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

pub enum MiseOutcome {
    /// Already on the system (path to the binary).
    #[allow(dead_code)]
    Present(String),
    /// We just installed it (path to the binary).
    Installed(String),
    /// Not present and could not be installed (reason).
    Failed(String),
}

/// A usable mise binary, or `None`. Checks PATH first, then mise's default
/// per-OS install location — a freshly-installed mise lands outside the
/// current process's PATH, so "just installed" wouldn't otherwise be visible
/// until a new shell.
pub fn mise_bin() -> Option<String> {
    if let Some(p) = which("mise") {
        return Some(p);
    }
    default_locations()
        .into_iter()
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Append mise's shims and tool bins without changing the caller's PATH priority.
/// Cache by cwd because project mise.toml files can select different tools.
pub fn append_mise_path(base: Option<&OsStr>, cwd: &Path) -> Option<OsString> {
    static BINS: OnceLock<Mutex<std::collections::HashMap<PathBuf, Vec<PathBuf>>>> =
        OnceLock::new();
    let cache = BINS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let hit = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(cwd)
        .cloned();
    // Resolve outside the lock: `mise` is a subprocess (~100ms).
    let bins = hit.unwrap_or_else(|| {
        let bins = disk_cached_paths(&mise_cache_file(), cwd, MISE_PATH_TTL, || mise_paths(cwd));
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cwd.to_path_buf(), bins.clone());
        bins
    });
    append_paths(base, &bins)
}

/// Every agentflare invocation (hooks included) resolves mise paths, so the
/// per-process cache alone still costs one `mise bin-paths` spawn per command.
const MISE_PATH_TTL: std::time::Duration = std::time::Duration::from_secs(300);

fn mise_cache_file() -> PathBuf {
    agentflare_config::agentflare_dir().join("mise-bin-paths.json")
}

type PathCache = std::collections::HashMap<String, (u64, Vec<PathBuf>)>;

/// `compute()`'s result for `cwd`, reused across processes for `ttl` via a
/// small JSON file. Any read/write failure just falls through to `compute()`.
fn disk_cached_paths(
    file: &Path,
    cwd: &Path,
    ttl: std::time::Duration,
    compute: impl FnOnce() -> Vec<PathBuf>,
) -> Vec<PathBuf> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let key = cwd.to_string_lossy().into_owned();
    let mut cache: PathCache = std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if let Some((at, paths)) = cache.get(&key)
        && now.saturating_sub(*at) < ttl.as_secs()
    {
        return paths.clone();
    }
    let paths = compute();
    // Don't cache "mise missing/failed" for 5 minutes: installing mise should
    // take effect on the next command.
    if !paths.is_empty() {
        cache.retain(|_, (at, _)| now.saturating_sub(*at) < ttl.as_secs());
        cache.insert(key, (now, paths.clone()));
        if let Ok(json) = serde_json::to_vec(&cache) {
            let _ = std::fs::write(file, json);
        }
    }
    paths
}

fn mise_paths(cwd: &Path) -> Vec<PathBuf> {
    let Some(mise) = mise_bin() else {
        return Vec::new();
    };
    let mut paths = dirs::data_local_dir()
        .map(|dir| dir.join("mise").join("shims"))
        .into_iter()
        .collect::<Vec<_>>();
    let output = flare_process::command(&mise)
        .arg("bin-paths")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        paths.extend(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        );
    } else if let Ok(output) = flare_process::command(&mise)
        .args(["env", "--json"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        && output.status.success()
        && let Ok(vars) =
            serde_json::from_slice::<std::collections::HashMap<String, String>>(&output.stdout)
        && let Some(path) = vars.get("PATH")
    {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        paths.extend(
            std::env::split_paths(OsStr::new(path))
                .filter(|p| !std::env::split_paths(&inherited).any(|existing| existing == *p)),
        );
    }
    paths
}

fn append_paths(base: Option<&OsStr>, bins: &[PathBuf]) -> Option<OsString> {
    if bins.is_empty() {
        return None;
    }
    let mut paths: Vec<_> = base.into_iter().flat_map(std::env::split_paths).collect();
    let original_len = paths.len();
    for bin in bins {
        if !paths.iter().any(|path| path == bin) {
            paths.push(bin.clone());
        }
    }
    (paths.len() != original_len)
        .then(|| std::env::join_paths(paths).ok())
        .flatten()
}

/// Called before agentflare starts threads, so all ordinary child processes
/// inherit mise tools even when the launching shell has not activated mise.
pub fn init_mise_path() {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    if let Some(path) = append_mise_path(std::env::var_os("PATH").as_deref(), &cwd) {
        // SAFETY: main calls this before starting any threads.
        unsafe { std::env::set_var("PATH", path) };
    }
}

/// Ensure mise is available, installing it cross-platform if absent.
pub fn ensure_mise() -> MiseOutcome {
    if let Some(bin) = mise_bin() {
        return MiseOutcome::Present(bin);
    }
    if let Err(e) = install() {
        return MiseOutcome::Failed(e);
    }
    match mise_bin() {
        Some(bin) => MiseOutcome::Installed(bin),
        None => MiseOutcome::Failed(
            "mise installer reported success but the binary was not found on PATH \
             or its default install location — open a new shell and re-run, or see \
             https://mise.jdx.dev/installing-mise.html"
                .to_string(),
        ),
    }
}

/// mise's default install location per OS (see mise's own install docs):
/// `~/.local/bin/mise` on Unix, `%LOCALAPPDATA%\mise\bin\mise.exe` on Windows.
fn default_locations() -> Vec<PathBuf> {
    if cfg!(windows) {
        let local = std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join("AppData").join("Local"));
        vec![local.join("mise").join("bin").join("mise.exe")]
    } else {
        vec![home().join(".local").join("bin").join("mise")]
    }
}

fn install() -> Result<(), String> {
    if cfg!(windows) {
        install_windows()
    } else {
        install_unix()
    }
}

/// Official installer (https://mise.run) via curl, wget as a fallback for
/// curl-less minimal images. Installs to ~/.local/bin/mise.
fn install_unix() -> Result<(), String> {
    let cmd = if has("curl") {
        "curl -fsSL https://mise.run | sh"
    } else if has("wget") {
        "wget -qO- https://mise.run | sh"
    } else {
        return Err(
            "cannot install mise: neither curl nor wget is available. Install one, \
             or install mise manually per https://mise.jdx.dev/installing-mise.html"
                .to_string(),
        );
    };
    run_shell(cmd)
}

/// Windows has no official one-line install *script* (the PowerShell snippet in
/// mise's docs only wires activation), so use the package managers mise itself
/// recommends: winget, then scoop.
fn install_windows() -> Result<(), String> {
    if has("winget")
        && run_status(
            "winget",
            &[
                "install",
                "-e",
                "--id",
                "jdx.mise",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ],
        )
    {
        return Ok(());
    }
    if has("scoop") && run_status("scoop", &["install", "mise"]) {
        return Ok(());
    }
    Err(
        "cannot install mise on Windows: neither winget nor scoop succeeded. Install \
         one (or mise directly) per https://mise.jdx.dev/installing-mise.html"
            .to_string(),
    )
}

fn which(cmd: &str) -> Option<String> {
    let checker = if cfg!(windows) { "where" } else { "which" };
    let out = flare_process::command(checker)
        .arg(cmd)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

fn has(cmd: &str) -> bool {
    which(cmd).is_some()
}

fn run_shell(cmd: &str) -> Result<(), String> {
    let result = if cfg!(windows) {
        flare_process::command("cmd").args(["/c", cmd]).status()
    } else {
        flare_process::command("sh").args(["-c", cmd]).status()
    };
    match result {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("mise installer exited with {:?}", s.code())),
        Err(e) => Err(format!("failed to run mise installer: {e}")),
    }
}

fn run_status(cmd: &str, args: &[&str]) -> bool {
    flare_process::command(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_locations_are_platform_appropriate_and_nonempty() {
        let locs = default_locations();
        assert!(!locs.is_empty());
        let joined = locs.iter().map(|p| p.to_string_lossy()).collect::<String>();
        assert!(joined.contains("mise"));
        if cfg!(windows) {
            assert!(joined.ends_with("mise.exe"));
        }
    }

    #[test]
    fn which_returns_none_for_a_nonexistent_command() {
        assert!(which("definitely-not-a-real-binary-xyz-123").is_none());
    }

    #[test]
    fn mise_paths_append_once_after_existing_path() {
        let base = std::env::join_paths(["first", "second"]).unwrap();
        let bins = vec![
            PathBuf::from("second"),
            PathBuf::from("mise"),
            PathBuf::from("mise"),
        ];
        let result = append_paths(Some(&base), &bins).unwrap();
        assert_eq!(
            std::env::split_paths(&result).collect::<Vec<_>>(),
            vec![
                PathBuf::from("first"),
                PathBuf::from("second"),
                PathBuf::from("mise")
            ]
        );
        assert_eq!(append_paths(Some(&base), &[]), None);
        assert_eq!(append_paths(Some(&result), &bins), None);
    }

    #[test]
    fn disk_cache_reuses_fresh_entry_and_skips_empty_results() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("c.json");
        let cwd = Path::new("proj");
        let ttl = std::time::Duration::from_secs(300);
        let bins = vec![PathBuf::from("mise-bin")];
        let first = disk_cached_paths(&file, cwd, ttl, || bins.clone());
        assert_eq!(first, bins);
        // Fresh hit: compute must not run again.
        let second = disk_cached_paths(&file, cwd, ttl, || panic!("recomputed"));
        assert_eq!(second, bins);
        // Expired (ttl 0): recomputed.
        let third = disk_cached_paths(&file, cwd, std::time::Duration::ZERO, Vec::new);
        assert!(third.is_empty());
        // Empty results are never persisted.
        let other = tmp.path().join("d.json");
        assert!(disk_cached_paths(&other, cwd, ttl, Vec::new).is_empty());
        assert!(!other.exists());
    }
}
