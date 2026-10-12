// mbx (mr-boxington) integration (item #330): dispatched agents build Rust
// through mbx's shared store. mbx is optional — everything here degrades to
// plain cargo when it is absent.
use serde::Serialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const INSTALL_HINT: &str = "cargo install mbx --locked";

/// Directory holding mbx's standalone `cargo` shim (`mbx setup`), if installed.
fn shim_dir_in(data_dir: &Path) -> Option<PathBuf> {
    let dir = data_dir.join("mbx").join("bin");
    // `mbx setup` installs `cargo.exe` on Windows.
    dir.join(format!("cargo{}", std::env::consts::EXE_SUFFIX))
        .is_file()
        .then_some(dir)
}

/// `path` with `shim` prepended; `None` when there is nothing to change.
fn prepend_dir(shim: Option<PathBuf>, path: Option<OsString>) -> Option<OsString> {
    let shim = shim?;
    let rest = path.unwrap_or_default();
    std::env::join_paths(std::iter::once(shim).chain(std::env::split_paths(&rest))).ok()
}

/// `base` (the agent's effective `PATH`) with mbx's shim prepended so plain
/// `cargo` resolves to it. `None` (leave `PATH` untouched, plain cargo) when
/// the shim isn't installed or mbx itself is gone (an orphaned shim would fail
/// instead of falling back). The sandbox inherits the child's environment, so
/// it sees the same value.
pub fn agent_path(base: Option<OsString>) -> Option<OsString> {
    installed()?;
    prepend_dir(shim_dir_in(&dirs::data_local_dir()?), base)
}

fn installed() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let base = std::env::var_os("PATH");
    let path = crate::mise_install::append_mise_path(base.as_deref(), &cwd).or(base);
    let on_path = path.and_then(|p| find_mbx(&p));
    on_path.or_else(|| {
        let c = dirs::home_dir()?
            .join(".cargo/bin")
            .join(format!("mbx{}", std::env::consts::EXE_SUFFIX));
        c.is_file().then_some(c)
    })
}

fn find_mbx(path: &OsString) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(format!("mbx{}", std::env::consts::EXE_SUFFIX)))
        .find(|candidate| candidate.is_file())
}

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("MBX_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| Some(dirs::cache_dir()?.join("mbx")))
}

#[derive(Debug, Serialize)]
pub struct MbxStatus {
    pub installed: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    /// Writable from inside the job sandbox profile (`None` when mbx is absent).
    pub cache_writable_in_sandbox: Option<bool>,
    /// False when bwrap is unavailable, so the writability probe ran on the host.
    pub sandboxed: bool,
    pub shim_active: bool,
    pub hint: Option<String>,
}

impl MbxStatus {
    pub fn collect() -> Self {
        let installed = installed();
        let cache_dir = cache_dir();
        let (writable, sandboxed) = match (&installed, &cache_dir) {
            (Some(_), Some(dir)) => {
                let (writable, sandboxed) = probe_sandbox_writable(dir);
                (Some(writable), sandboxed)
            }
            _ => (None, true),
        };
        MbxStatus {
            hint: installed.is_none().then(|| {
                format!("mbx not found — builds use plain cargo; install: {INSTALL_HINT}")
            }),
            installed,
            cache_dir,
            cache_writable_in_sandbox: writable,
            sandboxed,
            shim_active: agent_path(std::env::var_os("PATH")).is_some(),
        }
    }

    pub fn ok(&self) -> bool {
        self.installed.is_none() || self.cache_writable_in_sandbox == Some(true)
    }

    pub fn format_text(&self) -> String {
        let Some(bin) = &self.installed else {
            return format!("  mbx: not installed — plain cargo\n    hint: {INSTALL_HINT}");
        };
        let cache = self
            .cache_dir
            .as_deref()
            .map_or("?".into(), |p| p.display().to_string());
        let rw = match (self.cache_writable_in_sandbox, self.sandboxed) {
            (Some(true), true) => "writable in sandbox",
            (Some(true), false) => "writable (no sandbox available)",
            _ => "NOT writable in sandbox — set MBX_CACHE_DIR to ~/.cache/mbx or ~/.agentflare/…",
        };
        let shim = if self.shim_active {
            "cargo shim on agent PATH"
        } else {
            "cargo shim not installed (`mbx setup`) — agents use `mbx build|test` explicitly"
        };
        format!(
            "  mbx: {}\n    cache: {cache} ({rw})\n    {shim}",
            bin.display()
        )
    }
}

/// Runs `test -w <dir>` through the job sandbox profile; returns
/// `(writable, actually_sandboxed)`.
fn probe_sandbox_writable(dir: &Path) -> (bool, bool) {
    let args = vec!["-w".to_string(), dir.display().to_string()];
    let (cmd, args) = agentflare_jobs::sandbox::wrap("test", &args, None, false, None);
    let sandboxed = cmd != "test";
    let ok = std::process::Command::new(&cmd)
        .args(&args)
        .status()
        .is_ok_and(|s| s.success());
    (ok, sandboxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_dir_requires_cargo_shim_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(shim_dir_in(tmp.path()), None);
        let bin = tmp.path().join("mbx").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert_eq!(shim_dir_in(tmp.path()), None);
        std::fs::write(
            bin.join(format!("cargo{}", std::env::consts::EXE_SUFFIX)),
            "",
        )
        .unwrap();
        assert_eq!(shim_dir_in(tmp.path()), Some(bin));
    }

    #[test]
    fn installed_name_uses_platform_executable_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp
            .path()
            .join(format!("mbx{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&bin, "").unwrap();
        let path = std::env::join_paths([tmp.path()]).unwrap();
        assert_eq!(find_mbx(&path), Some(bin));
    }

    #[test]
    fn prepend_dir_puts_shim_first_and_noops_without_one() {
        // Built with the platform's own separator: `:` on Unix, `;` on Windows.
        let path = std::env::join_paths(["/usr/bin", "/bin"]).ok();
        assert_eq!(prepend_dir(None, path.clone()), None);
        let out = prepend_dir(Some(PathBuf::from("/shims")), path).unwrap();
        let expected = std::env::join_paths(["/shims", "/usr/bin", "/bin"]).unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn missing_mbx_status_is_ok_and_prints_install_hint() {
        let s = MbxStatus {
            installed: None,
            cache_dir: None,
            cache_writable_in_sandbox: None,
            sandboxed: true,
            shim_active: false,
            hint: Some(INSTALL_HINT.into()),
        };
        assert!(s.ok());
        assert!(s.format_text().contains(INSTALL_HINT));
    }

    #[test]
    fn unwritable_cache_is_not_ok() {
        let s = MbxStatus {
            installed: Some("/x/mbx".into()),
            cache_dir: Some("/ro".into()),
            cache_writable_in_sandbox: Some(false),
            sandboxed: true,
            shim_active: true,
            hint: None,
        };
        assert!(!s.ok());
        assert!(s.format_text().contains("NOT writable"));
    }
}
