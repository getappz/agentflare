//! Shared, policy-free atomic-write mechanics.
//!
//! agentflare writes files whose truncation on a crash mid-write would be a
//! real problem — auth vault credential files (`auth.rs`) and the user's own
//! shell profile (`alias.rs`'s managed alias block) — using plain
//! `fs::write`, which is not crash-atomic: a process killed between the
//! `open` and the `write` completing leaves a truncated (or zero-byte) file
//! in place of the original.
//!
//! This module provides one audited mechanism: a same-directory temp file +
//! `rename`, with an in-place-overwrite fallback when the directory is
//! read-only but the file inode itself is writable. Ported from lean-ctx's
//! `core/atomic_fs.rs`.

use std::borrow::Cow;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn invalid_input(msg: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, msg)
}

/// Resolves one level of symlink indirection so an atomic write lands on the
/// real file `path` points at, not on a new plain file replacing the symlink
/// itself — `rename()` does not follow a symlink destination, it unlinks it
/// and puts the new file in its place. A relative link target is resolved
/// against the symlink's own parent directory. Non-symlinks (and symlinks
/// this can't stat, e.g. a dangling one) pass through unchanged.
fn resolve_symlink_target(path: &Path) -> Cow<'_, Path> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Cow::Borrowed(path);
    };
    if !meta.file_type().is_symlink() {
        return Cow::Borrowed(path);
    }
    let Ok(target) = std::fs::read_link(path) else {
        return Cow::Borrowed(path);
    };
    if target.is_absolute() {
        Cow::Owned(target)
    } else {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        Cow::Owned(parent.join(target))
    }
}

/// Durable, crash-atomic write: a temp file in the **same directory** as `path`
/// (after resolving one level of symlink, so a symlinked target is written
/// through rather than replaced) followed by `rename` over the target.
/// Requires write permission on the parent directory; the read-only-directory
/// fallback is handled by [`write_bytes_with_fallback`].
///
/// When `permissions` is `None` and the target already exists, its current
/// permissions are carried over to the replacement — otherwise the new inode
/// would get the process's default (umask-masked) mode, silently loosening
/// e.g. a `0600` credential file to world-readable.
pub fn try_atomic_write(
    path: &Path,
    bytes: &[u8],
    permissions: Option<&std::fs::Permissions>,
) -> std::io::Result<()> {
    use std::io::Write;

    let resolved = resolve_symlink_target(path);
    let path: &Path = &resolved;

    let parent = path
        .parent()
        .ok_or_else(|| invalid_input("invalid path (no parent directory)"))?;
    let filename = path
        .file_name()
        .ok_or_else(|| invalid_input("invalid path (no filename)"))?
        .to_string_lossy();

    let owned_perms;
    let permissions = match permissions {
        Some(p) => Some(p),
        None => {
            owned_perms = std::fs::metadata(path).ok().map(|m| m.permissions());
            owned_perms.as_ref()
        }
    };

    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = parent.join(format!(".{filename}.agentflare.tmp.{pid}.{nanos}"));

    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        let _ = f.flush();
        let _ = f.sync_all();
    }

    if let Some(perms) = permissions {
        let _ = std::fs::set_permissions(&tmp, perms.clone());
    }

    // std::fs::rename already replaces an existing destination on every
    // platform we build for (including Windows, via MoveFileExW's
    // MOVEFILE_REPLACE_EXISTING) — no separate pre-removal needed, and one
    // would only open a window where neither the old nor new file exists.
    if let Err(e) = std::fs::rename(&tmp, path) {
        // Don't leave a half-written temp behind before the caller decides
        // whether to fall back.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// In-place overwrite of an existing file inode (`O_WRONLY|O_TRUNC`, plus
/// `O_NOFOLLOW` on Unix). Works when the parent directory is read-only but the
/// file itself is writable. Not crash-atomic — used only as a fallback when the
/// atomic path is impossible.
pub fn in_place_overwrite(
    path: &Path,
    bytes: &[u8],
    permissions: Option<&std::fs::Permissions>,
) -> std::io::Result<()> {
    use std::io::Write;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOFOLLOW: a symlink swapped in after the caller's checks must never
        // be followed here.
        opts.custom_flags(libc::O_NOFOLLOW);
    }

    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    let _ = f.flush();
    let _ = f.sync_all();

    if let Some(perms) = permissions {
        let _ = std::fs::set_permissions(path, perms.clone());
    }
    Ok(())
}

/// True for errors that mean "this directory won't accept create/rename" even
/// though the target file may be writable: `EROFS` (read-only fs) plus
/// `EACCES`/`EPERM` (directory write denied).
pub fn is_readonly_dir_error(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(
            e.raw_os_error(),
            Some(libc::EROFS | libc::EACCES | libc::EPERM)
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Atomic write with the read-only-directory in-place fallback. Tries the
/// crash-atomic temp+rename first; if that fails because the *directory* is
/// read-only/permission-denied but an existing file inode is writable, overwrite
/// it in place. `permissions`, when given, is applied to the written file.
pub fn write_bytes_with_fallback(
    path: &Path,
    bytes: &[u8],
    permissions: Option<&std::fs::Permissions>,
) -> Result<(), String> {
    match try_atomic_write(path, bytes, permissions) {
        Ok(()) => Ok(()),
        Err(e) if is_readonly_dir_error(&e) && path.is_file() => {
            in_place_overwrite(path, bytes, permissions).map_err(|fallback_err| {
                format!(
                    "atomic write failed ({e}); in-place fallback also failed: {fallback_err} ({})",
                    path.display()
                )
            })
        }
        Err(e) => Err(format!("atomic write failed: {e} ({})", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_dir_error_classification() {
        assert!(is_readonly_dir_error(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
        assert!(!is_readonly_dir_error(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
        #[cfg(unix)]
        {
            assert!(is_readonly_dir_error(&std::io::Error::from_raw_os_error(
                libc::EROFS
            )));
            assert!(is_readonly_dir_error(&std::io::Error::from_raw_os_error(
                libc::EACCES
            )));
            assert!(is_readonly_dir_error(&std::io::Error::from_raw_os_error(
                libc::EPERM
            )));
        }
    }

    #[test]
    fn try_atomic_write_creates_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.toml");
        try_atomic_write(&path, b"first", None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        // No leftover temp files.
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".agentflare.tmp."))
            .collect();
        assert!(strays.is_empty(), "temp file must not linger");
        try_atomic_write(&path, b"second", None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
    }

    #[cfg(unix)]
    #[test]
    fn try_atomic_write_preserves_existing_permissions_when_none_given() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        std::fs::write(&path, b"original").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        try_atomic_write(&path, b"updated", None).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "replacement must keep the original file's mode"
        );
    }

    #[cfg(unix)]
    #[test]
    fn try_atomic_write_writes_through_a_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&real, b"original").unwrap();
        symlink(&real, &link).unwrap();

        try_atomic_write(&link, b"updated", None).unwrap();

        assert!(link.is_symlink(), "the symlink itself must survive");
        assert_eq!(std::fs::read_link(&link).unwrap(), real);
        assert_eq!(std::fs::read(&real).unwrap(), b"updated");
    }

    #[cfg(unix)]
    #[test]
    fn in_place_overwrite_truncates_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.jsonc");
        std::fs::write(&path, b"longer original content").unwrap();
        in_place_overwrite(&path, b"short", None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
    }

    #[cfg(unix)]
    #[test]
    fn fallback_overwrites_when_parent_dir_is_readonly() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.toml");
        std::fs::write(&path, b"original").unwrap();
        // Read-only parent dir: temp+rename is impossible, but the file inode
        // stays writable, so the in-place fallback must succeed.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let res = write_bytes_with_fallback(&path, b"updated", None);
        let _ = std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700));
        res.expect("read-only-dir fallback must succeed");
        assert_eq!(std::fs::read(&path).unwrap(), b"updated");
    }

    #[test]
    fn write_bytes_with_fallback_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.txt");
        write_bytes_with_fallback(&path, b"hello", None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }
}
