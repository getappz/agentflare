//! Path helpers for every crate that compares, displays or stores paths.
//!
//! `std::fs::canonicalize` on Windows returns the verbatim form
//! (`\\?\C:\...`); git rejects it as an argument, it never equals the
//! `C:\...` form other code holds, and a plain prefix test between the two
//! silently fails. Every canonicalisation goes through [`canonical`] so no
//! caller has to remember that.

use std::path::{Path, PathBuf};

/// `path` in canonical form without a Windows verbatim prefix; the input
/// unchanged when it cannot be resolved (missing path, permission), so a
/// caller can carry on with what it has.
#[must_use]
pub fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `path` resolves to `root` itself or to somewhere under it.
#[must_use]
pub fn is_within(root: &Path, path: &Path) -> bool {
    canonical(path).starts_with(canonical(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_leaves_an_unresolvable_path_unchanged() {
        let missing = Path::new("/definitely/not/here/agentflare-paths-test");
        assert_eq!(canonical(missing), missing);
    }

    #[test]
    fn canonical_has_no_verbatim_prefix() {
        let dir = tempfile::TempDir::new().unwrap();
        let c = canonical(dir.path());
        assert!(c.is_absolute());
        assert!(!c.to_string_lossy().starts_with(r"\\?\"), "{c:?}");
    }

    #[cfg(unix)]
    #[test]
    fn is_within_sees_through_symlinks() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("sub")).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(is_within(&real, &link.join("sub")));
        assert!(is_within(&real, &link), "the root itself counts");
        assert!(!is_within(&real.join("sub"), &real));
    }
}
