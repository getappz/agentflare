//! Race-aware `$HOME` joins, ported from OpenShell's
//! `open_root_allowlist` (`openshell-isolation-interface/src/linux/landlock.rs`).
//!
//! OpenShell never trusts a blind listing of `/`: it opens exactly the named
//! root entries with `O_PATH | O_NOFOLLOW`, stats before and after, and
//! rejects symlinks, the private root, and raced replacements (dev/ino/mode
//! comparison). bwrap takes bind source paths literally, so a `$HOME` mount
//! that resolves through a symlink (or a `..` that walks above `$HOME`) would
//! bind the wrong directory into the sandbox. These helpers apply the same
//! discipline at the join level -- with one honest limitation: validation
//! and binding are separate operations (no open file descriptor is held
//! across to the bwrap exec, unlike OpenShell's fd-pinned rules), so a
//! same-user adversary swapping an ancestor between check and bind is not
//! prevented -- only detected as a wrong-directory bind, which the canonical-
//! starts-with-home check turns into a skip. In this threat model (the job
//! runner's own user) that fail-closed skip is the whole mitigation.
//!
//! 1. every `relative` component is validated (no `/`-in-component, `.`,
//!    `..`, empty, or NUL),
//! 2. an existing target must not itself be a symlink and must canonicalize
//!    to a path still under the canonicalized `$HOME`,
//! 3. callers skip the mount (with a `skipped_mount` event) instead of
//!    binding a wrong or escaped path.

use std::path::{Path, PathBuf};

/// Validate one path component of a `$HOME`-relative mount entry.
fn validate_component(component: &str) -> bool {
    !component.is_empty() && component != "." && component != ".." && !component.contains('\0')
}

/// Validate a whole `$HOME`-relative path (`a/b/c`): non-empty, relative
/// (no leading `/` or `\`), no empty/`.`/`..` components.
#[must_use]
pub fn is_valid_relative(relative: &str) -> bool {
    if relative.is_empty() || relative.starts_with('/') || relative.starts_with('\\') {
        return false;
    }
    let mut any = false;
    for component in relative.split('/') {
        if component.contains('\\') || !validate_component(component) {
            return false;
        }
        any = true;
    }
    any
}

/// Lexical join of a validated relative path onto `home`, `None` when
/// invalid. Does not touch the filesystem: use for not-yet-existing agent
/// state dirs (which become a plain tmpfs) and for pre-validating config
/// entries before any `exists()` check.
#[must_use]
pub fn join_validated_home_dir(home: &Path, relative: &str) -> Option<PathBuf> {
    if !is_valid_relative(relative) {
        return None;
    }
    Some(home.join(relative))
}

/// Resolve an *existing* `$HOME`-relative directory to its canonical path:
/// rejects symlinks at the final component, rejects escapes above `$HOME`,
/// returns `None` for missing/unresolvable/escaped targets so the caller
/// skips the bind instead of mounting the wrong directory.
#[must_use]
pub fn resolve_existing_home_dir(home: &Path, relative: &str) -> Option<PathBuf> {
    let joined = join_validated_home_dir(home, relative)?;
    let meta = std::fs::symlink_metadata(&joined).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    let canonical = std::fs::canonicalize(&joined).ok()?;
    let canonical_home = std::fs::canonicalize(home).ok()?;
    if !canonical.starts_with(&canonical_home) {
        return None;
    }
    Some(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_absolute_dotdot_and_empty() {
        assert!(!is_valid_relative(""));
        assert!(!is_valid_relative("/abs"));
        assert!(!is_valid_relative("a/../b"));
        assert!(!is_valid_relative("a/./b"));
        assert!(!is_valid_relative("a//b"));
        assert!(is_valid_relative(".cursor"));
        assert!(is_valid_relative(".local/share/opencode"));
    }

    #[test]
    fn resolves_existing_dir_canonically() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".cursor")).unwrap();
        let resolved = resolve_existing_home_dir(home.path(), ".cursor").unwrap();
        assert_eq!(
            resolved,
            std::fs::canonicalize(home.path().join(".cursor")).unwrap()
        );
    }

    #[test]
    fn missing_dir_resolves_to_none() {
        let home = tempfile::tempdir().unwrap();
        assert!(resolve_existing_home_dir(home.path(), ".nope").is_none());
        assert!(join_validated_home_dir(home.path(), ".nope").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_final_component_is_rejected() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join(".link")).unwrap();
        assert!(resolve_existing_home_dir(home.path(), ".link").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn dotdot_escape_through_symlinked_parent_is_rejected() {
        // `sub/../escaped` is lexically invalid here (.. rejected), but a
        // symlink *inside* an allowed dir pointing outside must also fail:
        // canonical("allowed/link") escapes home, so no bind.
        let home = tempfile::tempdir().unwrap();
        let allowed = home.path().join(".allowed");
        std::fs::create_dir(&allowed).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), allowed.join("link")).unwrap();
        assert!(resolve_existing_home_dir(home.path(), ".allowed/link").is_none());
    }
}
