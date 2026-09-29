//! Binary identity pins, ported from OpenShell's executable-identity pins
//! (`sandbox-limits.md`: 4,096 unique paths per supervisor lifetime).
//!
//! The previous matcher compared only the command's final path component
//! (`Path::file_name`), so any `PATH` entry shadowing `claude` with a
//! same-named binary inherited that profile's mounts. This module resolves
//! the command to a canonical executable path when possible and pins the
//! first-seen canonical path per binary name. A later different canonical
//! path with the same basename still matches (fail-open: refusing the mount
//! would break legitimate multi-install boxes) but emits an
//! `identity_mismatch` event so the shadowing is visible. The pin table is
//! bounded at 4,096 entries; past that, matching degrades to name-only with
//! an `identity_pins_exhausted` event.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Bound on pinned identities per process, matching OpenShell's 4,096
/// executable-identity-pin ceiling.
pub const MAX_IDENTITY_PINS: usize = 4096;

/// Resolved binary identity: basename plus best-effort canonical path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryIdentity {
    /// Final path component, e.g. `claude`.
    pub name: String,
    /// Canonicalized executable path when the file exists and resolves.
    pub canonical: Option<PathBuf>,
}

/// Outcome of recording an identity in the pin table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinOutcome {
    /// Whether this (name, canonical) pair is now the pinned entry.
    pub pinned: bool,
    /// Whether `name` was already pinned to a *different* canonical path.
    pub mismatch: bool,
    /// Whether the table was already full (nothing recorded).
    pub exhausted: bool,
}

fn pin_table() -> &'static Mutex<HashMap<String, String>> {
    static PINS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    PINS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Basename of `command` (`claude` for both `claude` and
/// `/usr/local/bin/claude`), `None` when there is no final component.
#[must_use]
pub fn binary_name(command: &str) -> Option<String> {
    Path::new(command)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
}

/// Resolve `command` to a [`BinaryIdentity`]: basename always, canonical
/// path when the target exists. Bare names (`claude`) are searched on
/// `PATH` with an executability check on Unix; paths containing a
/// separator are canonicalized directly.
#[must_use]
pub fn resolve(command: &str) -> Option<BinaryIdentity> {
    let name = binary_name(command)?;
    let candidate = if command.contains('/') || command.contains('\\') {
        PathBuf::from(command)
    } else {
        find_on_path(command)?
    };
    let canonical = std::fs::canonicalize(&candidate).ok();
    Some(BinaryIdentity { name, canonical })
}

/// Record `identity` in the pin table. First-seen canonical path wins;
/// later differing paths report `mismatch: true` without displacing the pin
/// (existing pins remain usable, as in OpenShell).
pub fn pin_identity(identity: &BinaryIdentity) -> PinOutcome {
    let Some(canonical) = identity.canonical.as_ref().map(|p| path_to_string(p)) else {
        return PinOutcome {
            pinned: false,
            mismatch: false,
            exhausted: false,
        };
    };
    let table = pin_table();
    let mut guard = table.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pinned) = guard.get(&identity.name) {
        return PinOutcome {
            pinned: false,
            mismatch: *pinned != canonical,
            exhausted: false,
        };
    }
    if guard.len() >= MAX_IDENTITY_PINS {
        return PinOutcome {
            pinned: false,
            mismatch: false,
            exhausted: true,
        };
    }
    guard.insert(identity.name.clone(), canonical);
    PinOutcome {
        pinned: true,
        mismatch: false,
        exhausted: false,
    }
}

/// Pure name comparison shared by the mount matcher: true when `command`'s
/// basename equals the profile's `binary_name`.
#[must_use]
pub fn names_match(command: &str, expected: &str) -> bool {
    binary_name(command).as_deref() == Some(expected)
}

/// Number of pinned identities (for tests and diagnostics).
#[must_use]
pub fn pin_count() -> usize {
    pin_table()
        .lock()
        .map(|g| g.len())
        .unwrap_or_else(|e| e.into_inner().len())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_name_takes_final_component() {
        assert_eq!(
            binary_name("/usr/local/bin/claude").as_deref(),
            Some("claude")
        );
        assert_eq!(binary_name("cursor-agent").as_deref(), Some("cursor-agent"));
        assert!(binary_name("").is_none());
    }

    #[test]
    fn names_match_compares_basenames() {
        assert!(names_match("/usr/bin/opencode", "opencode"));
        assert!(!names_match("/usr/bin/opencode", "claude"));
    }

    #[test]
    fn resolve_gives_canonical_path_for_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("pinned-agent");
        std::fs::write(&exe, b"").unwrap();
        let id = resolve(exe.to_str().unwrap()).unwrap();
        assert_eq!(id.name, "pinned-agent");
        assert!(id.canonical.is_some());
    }

    #[test]
    fn pin_first_canonical_wins_and_mismatch_reported() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        std::thread::current().id().hash(&mut h);
        let tag = format!("pin-test-{}", h.finish());
        let a = BinaryIdentity {
            name: tag.clone(),
            canonical: Some(PathBuf::from("/opt/a/agent")),
        };
        let b = BinaryIdentity {
            name: tag.clone(),
            canonical: Some(PathBuf::from("/opt/b/agent")),
        };
        assert!(pin_identity(&a).pinned);
        let second = pin_identity(&b);
        assert!(!second.pinned);
        assert!(second.mismatch);
    }
}
